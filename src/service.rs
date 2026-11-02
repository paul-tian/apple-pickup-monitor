use crate::{
    catalogs::ProductCatalogs,
    notifications::{CredentialStore, Credentials, Notifier},
    observations::{Observations, group_nearby_targets},
    progress::{FetchEvent, ProgressFetcher},
    store::{Database, Preferences, now_ms},
};
use apw_core::{
    apple::{AppleClient, Budget, ClientConfig},
    catalog::Catalog,
    model::{Availability, Category, REGIONS, Target, UnknownReason, region_by_locale},
    watcher::{Event, Watcher, WatcherConfig},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, RwLock, watch};

#[derive(Default)]
struct Health {
    next_check_at: Option<u64>,
    paced: bool,
    healthy: Option<bool>,
    notice: Option<String>,
}
pub struct App {
    pub db: Arc<Database>,
    pub watcher: Watcher,
    observations: RwLock<Observations>,
    generation: Arc<AtomicU64>,
    catalog: Catalog,
    products: ProductCatalogs,
    preferences: RwLock<Preferences>,
    credentials: RwLock<Credentials>,
    credential_store: CredentialStore,
    notifier: Notifier,
    health: RwLock<Health>,
    operation: Mutex<()>,
    credential_operation: Mutex<()>,
    refreshing: AtomicBool,
    manual_refreshing: AtomicBool,
    one_shot: AtomicBool,
    log_path: PathBuf,
    pub token: String,
    pub origin: String,
    pub quit: watch::Sender<bool>,
}
pub struct ApiError(StatusCode, String);
impl From<String> for ApiError {
    fn from(e: String) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
type ApiResult = Result<Json<Value>, ApiError>;
fn bad(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}
fn ok() -> ApiResult {
    Ok(Json(json!({"ok":true})))
}

fn retired_store(locale: &str, number: &str) -> bool {
    // Hornsby permanently closed; confirmed by the user on 2026-10-26.
    // The pinned upstream catalogue still contains it, so enforce this at every entry point.
    locale == "en_AU" && number == "R458"
}

fn load_active_targets(db: &Database) -> Result<Vec<Target>, String> {
    let mut targets = db.targets()?;
    let before = targets.len();
    targets.retain(|t| !retired_store(&t.locale, &t.store_number));
    if targets.len() != before {
        db.save_targets(&targets)?;
    }
    Ok(targets)
}

impl App {
    pub async fn new(
        db: Arc<Database>,
        cache_directory: PathBuf,
        log_path: PathBuf,
        namespace: &str,
        token: String,
        origin: String,
        quit: watch::Sender<bool>,
    ) -> Result<Arc<Self>, String> {
        let catalog = Catalog::new();
        let products = ProductCatalogs::new(cache_directory);
        let mut notice = None;
        let preferences = db.get::<Preferences>("preferences")?.unwrap_or_default();
        if !(30..=3600).contains(&preferences.interval_seconds) {
            return Err("Saved polling interval is invalid; expected 30–3600 seconds.".into());
        }
        // A new client has a fresh HTTP budget. Respect the previous process's
        // restart boundary (also the legacy cooldown representation), rather
        // than allowing Refresh now to reset it. Within this process ordinary
        // scheduled waits and actual cooldowns are tracked separately.
        let prior = db.get::<u64>("notBefore")?.unwrap_or(0);
        let cooldown = db.get::<u64>("stockCooldownUntil")?.unwrap_or(0);
        db.set("stockCooldownUntil", &prior.max(cooldown))?;
        let credential_store = CredentialStore::new(namespace);
        let loader = credential_store.clone();
        let credentials = match tokio::task::spawn_blocking(move || loader.load())
            .await
            .map_err(|e| e.to_string())?
        {
            Ok(c) => c,
            Err(error) => {
                notice = Some(error);
                Credentials::default()
            }
        };
        let client = AppleClient::new(ClientConfig {
            min_interval: Duration::from_secs(2),
            max_retries: 1,
            // Allow session warm-up plus a small initial batch. Each subsequent
            // request remains paced by the client's one-per-minute token refill.
            budget: Budget {
                capacity: 4,
                refill_every: Duration::from_secs(60),
            },
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
        let generation = Arc::new(AtomicU64::new(1));
        let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let (watcher, events, engine) = Watcher::new(
            ProgressFetcher::new(client, progress_tx, generation.clone()),
            WatcherConfig {
                interval: Duration::from_secs(preferences.interval_seconds),
                concurrency: 1,
                ..Default::default()
            },
        );
        tokio::spawn(engine);
        let mut targets = load_active_targets(&db)?;
        validate_targets(&targets)?;
        group_nearby_targets(&mut targets, &catalog);
        db.save_targets(&targets)?;
        let mut observations = Observations::default();
        observations.reset(&targets, 1);
        watcher.set_targets(targets).await;
        let app = Arc::new(Self {
            db,
            watcher,
            observations: RwLock::new(observations),
            generation,
            catalog,
            products,
            preferences: RwLock::new(preferences),
            credentials: RwLock::new(credentials),
            credential_store,
            notifier: Notifier::new()?,
            health: RwLock::new(Health {
                notice,
                ..Default::default()
            }),
            operation: Mutex::new(()),
            credential_operation: Mutex::new(()),
            refreshing: AtomicBool::new(false),
            manual_refreshing: AtomicBool::new(false),
            one_shot: AtomicBool::new(false),
            log_path,
            token,
            origin,
            quit,
        });
        tokio::spawn(consume_events(app.clone(), events, progress_rx));
        tokio::spawn(deliver_notifications(app.clone()));
        Ok(app)
    }
    async fn report(&self, message: String) {
        self.log(&message);
        self.health.write().await.notice = Some(message);
    }
    async fn replace_targets(&self, mut targets: Vec<Target>) -> Result<(), String> {
        let running = !self.one_shot.load(Ordering::Acquire) && self.watcher.is_running().await;
        self.watcher.stop().await;
        self.one_shot.store(false, Ordering::Release);
        self.manual_refreshing.store(false, Ordering::Release);
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        group_nearby_targets(&mut targets, &self.catalog);
        self.db.save_targets(&targets)?;
        self.observations.write().await.reset(&targets, generation);
        self.watcher.set_targets(targets.clone()).await;
        if running && !targets.is_empty() {
            self.start_after_reset().await;
        }
        let _ = self.watcher.snapshot().await;
        Ok(())
    }
    async fn start_after_reset(&self) {
        // The upstream cycle snapshot uses millisecond timestamps, whereas
        // fetch progress carries our generation. Cross a timestamp boundary so
        // even an immediate cached cooldown/error can finish this new round,
        // while the strict old-cycle rejection in Observations stays intact.
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.watcher.start().await;
    }
    fn log(&self, message: &str) {
        use std::io::Write;
        if let Ok(meta) = std::fs::metadata(&self.log_path)
            && meta.len() > 1024 * 1024
        {
            let _ = std::fs::rename(&self.log_path, self.log_path.with_extension("previous.log"));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(file, "{} {}", now_ms(), message.replace('\n', " "));
        }
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/styles.css",
            get(|| async {
                (
                    [("content-type", "text/css; charset=utf-8")],
                    include_str!("../web/styles.css"),
                )
            }),
        )
        .route("/api/state", get(state))
        .route("/api/regions", get(regions))
        .route("/api/catalog", get(catalog))
        .route("/api/catalog/refresh", post(refresh_catalog))
        .route("/api/targets", post(add_targets))
        .route("/api/targets/remove", post(remove_target))
        .route("/api/monitor", post(monitor))
        .route("/api/monitor/refresh", post(refresh_stock))
        .route("/api/settings", put(settings))
        .route("/api/language", put(notification_language))
        .route("/api/credentials", put(credentials))
        .route("/api/notifications/test", post(test_notification))
        .route("/api/quit", post(quit))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), protect))
        .with_state(app)
}
async fn protect(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let expected_host = app.origin.trim_start_matches("http://");
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if host != expected_host {
        return ApiError(
            StatusCode::FORBIDDEN,
            "Use the local dashboard address opened by the app.".into(),
        )
        .into_response();
    }
    if let Some(origin) = request.headers().get("origin")
        && origin.to_str().ok() != Some(app.origin.as_str())
    {
        return ApiError(
            StatusCode::FORBIDDEN,
            "Cross-origin requests are not permitted.".into(),
        )
        .into_response();
    }
    if request.uri().path().starts_with("/api/")
        && request
            .headers()
            .get("x-session-token")
            .and_then(|h| h.to_str().ok())
            != Some(app.token.as_str())
    {
        return ApiError(
            StatusCode::UNAUTHORIZED,
            "Reopen the dashboard by launching Apple Pickup Monitor.".into(),
        )
        .into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("content-security-policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"));
    response
}
async fn state(State(app): State<Arc<App>>) -> ApiResult {
    let one_shot = app.one_shot.load(Ordering::Acquire);
    let checking = app.watcher.is_running().await;
    let running = checking && !one_shot;
    let observations = app.observations.read().await;
    let targets:Vec<Value>=app.db.targets()?.iter().map(|target| {
        let s=observations.row(target);
        let phase=if checking { observations.check_phase(target) } else { None };
        let detail=if matches!(s.availability,Availability::Unknown(UnknownReason::NotYetChecked)) && checking {
            match phase {Some("checking")=>"Checking this area with Apple; this includes any request pacing.".into(),_=>"Queued while earlier areas are checked.".into()}
        }else{availability_detail(&s.availability)};
        json!({"key":s.target.key().to_string(),"target":s.target,"availability":s.availability,
            "checkPhase":phase,"availabilityDetail":detail,"lastCheckedMs":s.last_checked_ms,"consecutiveFailures":s.consecutive_failures})
    }).collect();
    let prefs = app.preferences.read().await;
    let health = app.health.read().await;
    let creds = app.credentials.read().await;
    Ok(Json(
        json!({"version":env!("APPLE_PICKUP_APP_VERSION"),"running":running,"intervalSeconds":prefs.interval_seconds,
            "targets":targets,"notifications":app.db.history()?,"nextCheckAt":if running {health.next_check_at}else{None},
            "progress":if checking {observations.progress.clone()}else{None},
            "refreshingStock":app.manual_refreshing.load(Ordering::Acquire),
            "nextManualCheckAt":manual_check_after(&app)?,
            "nextAllowedStartAt":app.db.get::<u64>("notBefore")?,
            "notificationLanguage":prefs.notification_language,
            "paced":health.paced,"healthy":health.healthy,"notice":health.notice,"catalogRefreshing":app.refreshing.load(Ordering::Relaxed),
            "settings":{"pushoverEnabled":prefs.pushover_enabled,"barkEnabled":prefs.bark_enabled,
                "pushoverConfigured":!creds.pushover_token.is_empty()&&!creds.pushover_user.is_empty(),"barkConfigured":!creds.bark_url.is_empty()}
        }),
    ))
}
fn availability_detail(a: &Availability) -> String {
    match a {
        Availability::InStock => "Available for pickup at the last check.".into(),
        Availability::OutOfStock => {
            "Apple reports no pickup availability for this configuration.".into()
        }
        Availability::Unknown(reason) => match reason {
            UnknownReason::NotYetChecked => "Waiting for the first check.".into(),
            UnknownReason::Blocked { .. } => {
                "Apple blocked the request. Monitoring will cool down before retrying.".into()
            }
            UnknownReason::RateLimited => {
                "Apple is limiting requests. Waiting before retrying.".into()
            }
            UnknownReason::SchemaDrift { .. } => {
                "Apple returned an unexpected response. Availability is unknown.".into()
            }
            UnknownReason::AppleError { .. } => {
                "Apple did not provide a pickup result. Check the product on Apple's website."
                    .into()
            }
            UnknownReason::Transport { .. } => {
                "The request could not complete. Check your internet connection.".into()
            }
            UnknownReason::PickupNotOpen { .. } => {
                "Apple has not opened pickup for this configuration yet.".into()
            }
        },
    }
}
fn default_locale() -> String {
    "en_AU".into()
}

#[derive(Deserialize)]
struct RegionQuery {
    #[serde(default = "default_locale")]
    locale: String,
}

fn validate_locale(locale: &str) -> Result<(), ApiError> {
    if region_by_locale(locale).is_none() {
        return Err(bad("This Apple store region is not supported."));
    }
    Ok(())
}

async fn regions() -> ApiResult {
    Ok(Json(
        json!({"regions": REGIONS.iter().map(|r| json!({"locale":r.locale,"title":r.title,"baseUrl":r.base_url})).collect::<Vec<_>>()}),
    ))
}

async fn catalog(State(app): State<Arc<App>>, Query(input): Query<RegionQuery>) -> ApiResult {
    validate_locale(&input.locale)?;
    let stores: Vec<_> = app
        .catalog
        .stores(&input.locale)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|s| !retired_store(&input.locale, &s.number))
        .collect();
    let snapshot = app.products.get(&input.locale, &app.catalog).await?;
    if let Some(warning) = &snapshot.warning {
        app.health.write().await.notice = Some(warning.clone());
    }
    Ok(Json(
        json!({"locale":input.locale,"stores":stores,"products":snapshot.products,"notice":snapshot.warning}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddTargets {
    #[serde(default = "default_locale")]
    locale: String,
    store_numbers: Vec<String>,
    part_numbers: Vec<String>,
}
async fn add_targets(State(app): State<Arc<App>>, Json(input): Json<AddTargets>) -> ApiResult {
    let _guard = app.operation.lock().await;
    validate_locale(&input.locale)?;
    if input.store_numbers.is_empty() || input.part_numbers.is_empty() {
        return Err(bad("Select at least one store and product."));
    }
    if input.store_numbers.len() > 30 || input.part_numbers.len() > 20 {
        return Err(bad("Select fewer stores or products."));
    }
    let products = app
        .products
        .get(&input.locale, &app.catalog)
        .await?
        .products;
    let mut targets = app.db.targets()?;
    for number in &input.store_numbers {
        if retired_store(&input.locale, number) {
            return Err(bad(
                "Apple Hornsby has permanently closed and cannot be monitored.",
            ));
        }
        let store = app
            .catalog
            .store_by_number(&input.locale, number)
            .ok_or_else(|| bad("The selected store was not found in this region."))?;
        for part in &input.part_numbers {
            let product = products
                .iter()
                .find(|p| p.part_number == *part)
                .ok_or_else(|| bad("The selected product was not found. Refresh the catalogue."))?;
            let target = Target {
                locale: input.locale.clone(),
                store_number: store.number.clone(),
                store_title: store.title.clone(),
                part_number: product.part_number.clone(),
                product_name: product.title.clone(),
                companion_part: product.companion_part.clone(),
                pickup_location: store.pickup_location(&input.locale),
            };
            if !targets.iter().any(|t| t.key() == target.key()) {
                targets.push(target);
            }
        }
    }
    validate_targets(&targets).map_err(bad)?;
    app.replace_targets(targets).await?;
    ok()
}
fn validate_targets(targets: &[Target]) -> Result<(), String> {
    if targets.len() > 100 {
        return Err("This prototype supports up to 100 store/product combinations.".into());
    }
    let mut parts: HashMap<&str, HashSet<&str>> = HashMap::new();
    for target in targets {
        if region_by_locale(&target.locale).is_none() {
            return Err("This Apple store region is not supported.".into());
        }
        if retired_store(&target.locale, &target.store_number) {
            return Err("Apple Hornsby has permanently closed and cannot be monitored.".into());
        }
        let regional_parts = parts.entry(&target.locale).or_default();
        regional_parts.insert(&target.part_number);
        if let Some(part) = &target.companion_part {
            regional_parts.insert(part);
        }
    }
    if parts
        .values()
        .any(|regional_parts| regional_parts.len() > 20)
    {
        return Err("Each region supports 20 distinct part numbers, including any required Apple Watch bands. Remove some products first.".into());
    }
    Ok(())
}
#[derive(Deserialize)]
struct RemoveTarget {
    key: String,
}
async fn remove_target(State(app): State<Arc<App>>, Json(input): Json<RemoveTarget>) -> ApiResult {
    let _guard = app.operation.lock().await;
    let mut targets = app.db.targets()?;
    targets.retain(|t| t.key().to_string() != input.key);
    app.replace_targets(targets).await?;
    ok()
}
#[derive(Deserialize)]
struct Monitor {
    running: bool,
}
async fn monitor(State(app): State<Arc<App>>, Json(input): Json<Monitor>) -> ApiResult {
    let _guard = app.operation.lock().await;
    if input.running && app.one_shot.swap(false, Ordering::AcqRel) {
        // Starting automatic monitoring during a manual round promotes that
        // same round; it must not start another fetch or stop on completion.
        return ok();
    }
    if input.running && !app.watcher.is_running().await {
        if app.db.targets()?.is_empty() {
            return Err(bad("Add a store and product before starting."));
        }
        let not_before = app.db.get::<u64>("notBefore")?.unwrap_or(0);
        if not_before > now_ms() {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Allow {} seconds before restarting so Apple's request cooldown is respected.",
                    (not_before - now_ms()) / 1000 + 1
                ),
            ));
        }
        app.health.write().await.next_check_at = None;
        let generation = app.generation.fetch_add(1, Ordering::AcqRel) + 1;
        app.observations
            .write()
            .await
            .reset(&app.db.targets()?, generation);
        app.db.set("notBefore", &(now_ms() + 120_000))?;
        app.start_after_reset().await;
    } else if !input.running {
        app.watcher.stop().await;
        app.one_shot.store(false, Ordering::Release);
        app.manual_refreshing.store(false, Ordering::Release);
        let generation = app.generation.fetch_add(1, Ordering::AcqRel) + 1;
        app.observations
            .write()
            .await
            .reset(&app.db.targets()?, generation);
    }
    ok()
}

fn manual_check_after(app: &App) -> Result<u64, String> {
    Ok(app
        .db
        .get::<u64>("manualNotBefore")?
        .unwrap_or(0)
        .max(app.db.get::<u64>("stockCooldownUntil")?.unwrap_or(0)))
}

async fn refresh_stock(State(app): State<Arc<App>>) -> ApiResult {
    let _guard = app.operation.lock().await;
    let targets = app.db.targets()?;
    if targets.is_empty() {
        return Err(bad("Add a store and product before refreshing."));
    }
    if app.manual_refreshing.load(Ordering::Acquire) {
        // Repeated clicks join the existing manual round, never queue another.
        return ok();
    }
    let running = app.watcher.is_running().await;
    let next = app.health.read().await.next_check_at;
    let checking = running
        && (app.observations.read().await.progress.is_some()
            || next.is_none_or(|at| at <= now_ms()));
    if !checking && manual_check_after(&app)? > now_ms() {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Wait for the current request cooldown before refreshing again.".into(),
        ));
    }
    if !checking {
        app.db.set("notBefore", &(now_ms() + 120_000))?;
    }
    app.db.set("manualNotBefore", &(now_ms() + 30_000))?;
    app.manual_refreshing.store(true, Ordering::Release);
    if checking {
        // An automatic round already in flight satisfies the manual request.
        // Keep its requests, progress, pacing and generation intact.
        return ok();
    }

    // The upstream actor has no check-now command. Stop an idle schedule and
    // restart the same actor/client, keeping its actual HTTP request budget.
    // Real backoff is persisted separately and was checked above.
    app.watcher.stop().await;
    let generation = app.generation.fetch_add(1, Ordering::AcqRel) + 1;
    app.observations.write().await.reset(&targets, generation);
    app.health.write().await.next_check_at = None;
    app.one_shot.store(!running, Ordering::Release);
    app.start_after_reset().await;
    ok()
}
async fn settings(State(app): State<Arc<App>>, Json(input): Json<Preferences>) -> ApiResult {
    let _guard = app.operation.lock().await;
    if !["en", "zh-Hans", "zh-Hant"].contains(&input.notification_language.as_str()) {
        return Err(bad("Choose a supported notification language."));
    }
    if !(30..=3600).contains(&input.interval_seconds) {
        return Err(bad(
            "Choose a polling interval between 30 and 3600 seconds.",
        ));
    }
    let creds = app.credentials.read().await;
    if input.pushover_enabled && (creds.pushover_token.is_empty() || creds.pushover_user.is_empty())
    {
        return Err(bad("Save both Pushover keys before enabling Pushover."));
    }
    if input.bark_enabled && creds.bark_url.is_empty() {
        return Err(bad("Save a Bark URL before enabling Bark."));
    }
    app.db.set("preferences", &input)?;
    app.watcher
        .set_interval(Duration::from_secs(input.interval_seconds))
        .await;
    *app.preferences.write().await = input;
    ok()
}
#[derive(Deserialize)]
struct LanguageInput {
    language: String,
}
async fn notification_language(
    State(app): State<Arc<App>>,
    Json(input): Json<LanguageInput>,
) -> ApiResult {
    if !["en", "zh-Hans", "zh-Hant"].contains(&input.language.as_str()) {
        return Err(bad("This notification language is not supported."));
    }
    let _guard = app.operation.lock().await;
    let mut prefs = app.preferences.read().await.clone();
    prefs.notification_language = input.language;
    app.db.set("preferences", &prefs)?;
    *app.preferences.write().await = prefs;
    ok()
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialInput {
    pushover_token: Option<String>,
    pushover_user: Option<String>,
    bark_url: Option<String>,
}
async fn credentials(State(app): State<Arc<App>>, Json(input): Json<CredentialInput>) -> ApiResult {
    // Keychain may wait on a user prompt. It must not hold the monitor's
    // controller lock, which is needed to finish/cancel a one-shot refresh.
    let _guard = app.credential_operation.lock().await;
    let mut value = app.credentials.read().await.clone();
    for (dest, source) in [
        (&mut value.pushover_token, input.pushover_token),
        (&mut value.pushover_user, input.pushover_user),
        (&mut value.bark_url, input.bark_url),
    ] {
        if let Some(source) = source.filter(|s| !s.trim().is_empty()) {
            *dest = source.trim().into();
        }
    }
    value.validate().map_err(bad)?;
    let store = app.credential_store.clone();
    let saved = value.clone();
    tokio::task::spawn_blocking(move || store.save(&saved))
        .await
        .map_err(|e| e.to_string())??;
    *app.credentials.write().await = value;
    ok()
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestNotification {
    channel: String,
    pushover_token: Option<String>,
    pushover_user: Option<String>,
    bark_url: Option<String>,
}
impl TestNotification {
    /// Use drafts for this test only. Blank fields retain saved values, just as
    /// they do when saving, and another provider's edits cannot affect the test.
    fn credentials_for_test(&self, saved: &Credentials) -> Credentials {
        let mut credentials = saved.clone();
        let fields = match self.channel.as_str() {
            "pushover" => vec![
                (
                    &mut credentials.pushover_token,
                    self.pushover_token.as_deref(),
                ),
                (
                    &mut credentials.pushover_user,
                    self.pushover_user.as_deref(),
                ),
            ],
            "bark" => vec![(&mut credentials.bark_url, self.bark_url.as_deref())],
            _ => vec![],
        };
        for (destination, draft) in fields {
            if let Some(value) = draft.map(str::trim).filter(|value| !value.is_empty()) {
                *destination = value.to_owned();
            }
        }
        credentials
    }
}
async fn test_notification(
    State(app): State<Arc<App>>,
    Json(input): Json<TestNotification>,
) -> ApiResult {
    if !["pushover", "bark"].contains(&input.channel.as_str()) {
        return Err(bad("Select Pushover or Bark."));
    }
    let creds = input.credentials_for_test(&*app.credentials.read().await);
    let language = app.preferences.read().await.notification_language.clone();
    let (title, message) = crate::store::localized_test_notification(&language);
    let result = app
        .notifier
        .send(
            &input.channel,
            &creds,
            title,
            message,
            Some("https://www.apple.com/shop"),
        )
        .await;
    app.db.record_test(&input.channel, &result)?;
    result.map_err(bad)?;
    ok()
}
#[derive(Deserialize)]
struct RefreshCatalog {
    #[serde(default = "default_locale")]
    locale: String,
    category: Category,
}
async fn refresh_catalog(
    State(app): State<Arc<App>>,
    Json(input): Json<RefreshCatalog>,
) -> ApiResult {
    validate_locale(&input.locale)?;
    let category_key = serde_json::to_string(&input.category).map_err(|e| e.to_string())?;
    let cooldown_key = format!("catalogNotBefore:{}:{category_key}", input.locale);
    let not_before = app.db.get::<u64>(&cooldown_key)?.unwrap_or(0);
    if not_before > now_ms() {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "The catalogue was refreshed recently. Allow five minutes between refreshes.".into(),
        ));
    }
    if app.refreshing.swap(true, Ordering::SeqCst) {
        return Err(bad("A catalogue refresh is already in progress."));
    }
    if let Err(error) = app.db.set(&cooldown_key, &(now_ms() + 300_000)) {
        app.refreshing.store(false, Ordering::SeqCst);
        return Err(error.into());
    }
    tokio::spawn(async move {
        let result = async {
            let http = reqwest::Client::builder().timeout(Duration::from_secs(25)).build().map_err(|e| e.to_string())?;
            let region = region_by_locale(&input.locale).ok_or("This Apple store region is not supported.")?;
            app.catalog.refresh_products(region, Some(input.category), &http).await
                .map_err(|_| "Apple's catalogue refresh could not complete. Your saved catalogue is unchanged.".to_string())?;
            let updated = app.catalog.products(&input.locale).map_err(|e| e.to_string())?;
            app.products.replace_category(&input.locale, input.category, updated, &app.catalog).await
        }.await;
        match result {
            Ok(()) => {
                app.health.write().await.notice =
                    Some(format!("Product catalogue refreshed for {}.", input.locale))
            }
            Err(e) => app.report(e).await,
        }
        app.refreshing.store(false, Ordering::SeqCst);
    });
    ok()
}
async fn quit(State(app): State<Arc<App>>) -> ApiResult {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let _ = app.quit.send(true);
    });
    ok()
}
async fn consume_events(
    app: Arc<App>,
    mut events: tokio::sync::mpsc::Receiver<Event>,
    mut progress: tokio::sync::mpsc::UnboundedReceiver<FetchEvent>,
) {
    loop {
        // Fetch completion is enqueued before the core can publish its cycle.
        // Drain it first so reconciliation can never inflate a response timestamp.
        let event = tokio::select! {
            biased;
            Some(fetch) = progress.recv() => { consume_progress(&app, fetch).await; continue; }
            event = events.recv() => match event {Some(event)=>event,None=>break},
        };
        if let Event::CycleComplete {
            next_check_in_secs,
            paced,
            healthy,
            snapshot,
        } = event
        {
            let _guard = app.operation.lock().await;
            if !app.observations.read().await.accepts_cycle(&snapshot) {
                continue;
            }
            let one_shot = app.one_shot.load(Ordering::Acquire);
            let mut prefs = app.preferences.read().await.clone();
            if one_shot {
                // A refresh while paused updates the displayed/confirmed stock
                // without releasing or creating paused notification jobs.
                prefs.pushover_enabled = false;
                prefs.bark_enabled = false;
            }
            let missing = app.observations.write().await.finish_cycle(&snapshot);
            if let Err(error) = app.db.observe(&missing, &prefs) {
                app.report(format!("Could not save stock results: {error}"))
                    .await;
            }
            let blocked = snapshot.iter().any(|s| {
                matches!(
                    s.availability,
                    Availability::Unknown(UnknownReason::Blocked { .. })
                )
            });
            let next = now_ms() + next_check_in_secs * 1000;
            let not_before = if blocked {
                // The core's block state is process-local. A restart uses the
                // longest core cooldown rather than resetting an escalated block.
                next.max(now_ms() + 30 * 60_000)
            } else {
                next
            };
            if let Err(error) = app.db.set("notBefore", &not_before) {
                app.report(error).await;
            }
            let rate_limited = snapshot.iter().any(|s| {
                matches!(
                    s.availability,
                    Availability::Unknown(UnknownReason::RateLimited)
                )
            });
            let all_failed =
                !snapshot.is_empty() && snapshot.iter().all(|s| s.availability.is_failure());
            let cooldown = if blocked {
                not_before
            } else if rate_limited || all_failed {
                next.max(
                    app.db
                        .get::<u64>("stockCooldownUntil")
                        .ok()
                        .flatten()
                        .unwrap_or(0),
                )
            } else {
                0
            };
            if let Err(error) = app.db.set("stockCooldownUntil", &cooldown) {
                app.report(error).await;
            }
            if one_shot {
                app.watcher.stop().await;
                app.one_shot.store(false, Ordering::Release);
            }
            app.manual_refreshing.store(false, Ordering::Release);
            let mut health = app.health.write().await;
            health.next_check_at = (!one_shot).then_some(next);
            health.paced = paced;
            health.healthy = Some(healthy);
            if !healthy {
                health.notice=Some(if blocked {"Apple is temporarily blocking checks. The monitor will wait before trying again."}else{"Some checks could not confirm availability. See the affected products below."}.into());
            } else if health.notice.as_deref().is_some_and(|n| {
                n.starts_with("Some checks") || n.starts_with("Apple is temporarily")
            }) {
                health.notice = None;
            }
        }
    }
    app.report("The monitoring engine stopped unexpectedly. Quit and reopen the app.".into())
        .await;
}
async fn consume_progress(app: &App, event: FetchEvent) {
    let _guard = app.operation.lock().await;
    let generation = match &event {
        FetchEvent::Started { generation, .. } | FetchEvent::Finished { generation, .. } => {
            *generation
        }
    };
    if generation != app.generation.load(Ordering::Acquire) || !app.watcher.is_running().await {
        return;
    }
    let targets = match app.db.targets() {
        Ok(t) => t,
        Err(e) => {
            app.report(e).await;
            return;
        }
    };
    let started = matches!(event, FetchEvent::Started { .. });
    let rows = app.observations.write().await.apply(event, &targets);
    if started {
        app.health.write().await.next_check_at = None;
    }
    if rows.is_empty() {
        return;
    }
    let mut prefs = app.preferences.read().await.clone();
    if app.one_shot.load(Ordering::Acquire) {
        prefs.pushover_enabled = false;
        prefs.bark_enabled = false;
    }
    if let Err(error) = app.db.observe(&rows, &prefs) {
        app.report(format!("Could not save stock results: {error}"))
            .await;
    }
    let failed = rows.iter().any(|s| s.availability.is_failure());
    let blocked = rows.iter().any(|s| {
        matches!(
            s.availability,
            Availability::Unknown(UnknownReason::Blocked { .. })
        )
    });
    let rate_limited = rows.iter().any(|s| {
        matches!(
            s.availability,
            Availability::Unknown(UnknownReason::RateLimited)
        )
    });
    if blocked || rate_limited {
        let until = if blocked {
            now_ms() + 30 * 60_000
        } else {
            now_ms() + prefs.interval_seconds.max(60) * 2_000
        };
        for key in ["notBefore", "stockCooldownUntil"] {
            let until = app
                .db
                .get::<u64>(key)
                .ok()
                .flatten()
                .unwrap_or(0)
                .max(until);
            if let Err(error) = app.db.set(key, &until) {
                app.report(error).await;
            }
        }
    }
    if failed {
        app.health.write().await.notice = Some(
            "Some checks could not confirm availability. See the affected products below.".into(),
        );
    }
    app.log(&format!(
        "Received {} pickup results{}",
        rows.len(),
        if failed {
            " (includes unknown results)"
        } else {
            ""
        }
    ));
}
async fn deliver_notifications(app: Arc<App>) {
    let mut quit = app.quit.subscribe();
    loop {
        tokio::select! {_=quit.changed()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
        // Serialize dispatch with removal/pause so committed changes take effect
        // before any subsequent provider request begins.
        let _guard = app.operation.lock().await;
        if app.one_shot.load(Ordering::Acquire) || !app.watcher.is_running().await {
            continue;
        }
        let job = match app.db.next_delivery() {
            Ok(Some(job)) => job,
            Ok(None) => continue,
            Err(e) => {
                app.report(e).await;
                continue;
            }
        };
        let prefs = app.preferences.read().await.clone();
        if !app.db.is_delivery_current(&job).unwrap_or(false) {
            continue;
        }
        let enabled = match job.channel.as_str() {
            "pushover" => prefs.pushover_enabled,
            "bark" => prefs.bark_enabled,
            _ => false,
        };
        let result = if enabled {
            let creds = app.credentials.read().await.clone();
            app.notifier
                .send(
                    &job.channel,
                    &creds,
                    &job.title,
                    &job.message,
                    job.url.as_deref(),
                )
                .await
        } else {
            Err("This notification channel is disabled.".into())
        };
        if let Err(error) = app.db.finish_delivery(&job, &result) {
            app.report(error).await;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request as HttpRequest,
    };
    use tower::ServiceExt;

    async fn test_app() -> (Arc<App>, tempfile::TempDir) {
        test_app_with_fetcher(AppleClient::new(Default::default()).unwrap()).await
    }
    async fn test_app_with_fetcher<F: apw_core::apple::Fetcher>(
        fetcher: F,
    ) -> (Arc<App>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.sqlite")).unwrap());
        let catalog = Catalog::new();
        let generation = Arc::new(AtomicU64::new(1));
        let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let (watcher, events) = Watcher::spawn(
            ProgressFetcher::new(fetcher, progress_tx, generation.clone()),
            Default::default(),
        );
        let (quit, _) = watch::channel(false);
        let app = Arc::new(App {
            db,
            watcher,
            observations: RwLock::new(Observations::default()),
            generation,
            catalog,
            products: ProductCatalogs::new(dir.path().join("cache")),
            preferences: RwLock::new(Preferences::default()),
            credentials: RwLock::new(Credentials::default()),
            credential_store: CredentialStore::new("test-no-keychain-access"),
            notifier: Notifier::new().unwrap(),
            health: RwLock::new(Health::default()),
            operation: Mutex::new(()),
            credential_operation: Mutex::new(()),
            refreshing: AtomicBool::new(false),
            manual_refreshing: AtomicBool::new(false),
            one_shot: AtomicBool::new(false),
            log_path: dir.path().join("test.log"),
            token: "test-secret".into(),
            origin: "http://127.0.0.1:8765".into(),
            quit,
        });
        tokio::spawn(consume_events(app.clone(), events, progress_rx));
        (app, dir)
    }

    #[derive(Clone)]
    struct ManualFetcher {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        permits: Arc<tokio::sync::Semaphore>,
        failure: &'static str,
    }
    impl ManualFetcher {
        fn new(failure: &'static str) -> Self {
            Self {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                permits: Arc::new(tokio::sync::Semaphore::new(0)),
                failure,
            }
        }
        async fn result(
            &self,
            store: &str,
            parts: &[String],
        ) -> Result<apw_core::apple::StoreAvailability, apw_core::apple::ApiError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.permits.acquire().await.unwrap().forget();
            match self.failure {
                "blocked" => return Err(apw_core::apple::ApiError::Blocked("fixture".into())),
                "rate_limited" => {
                    return Err(apw_core::apple::ApiError::RateLimited("fixture".into()));
                }
                "transport" => return Err(apw_core::apple::ApiError::Transport("fixture".into())),
                _ => {}
            }
            Ok(apw_core::apple::StoreAvailability {
                store_number: store.into(),
                store_name: store.into(),
                parts: parts
                    .iter()
                    .map(|part| {
                        (
                            part.clone(),
                            apw_core::apple::PartStatus {
                                part_number: part.clone(),
                                availability: Availability::InStock,
                                product_title: None,
                                pickup_display: "available".into(),
                            },
                        )
                    })
                    .collect(),
            })
        }
    }
    impl apw_core::apple::Fetcher for ManualFetcher {
        async fn pickup_message(
            &self,
            _region: &'static apw_core::model::Region,
            store: &str,
            parts: &[String],
        ) -> Result<apw_core::apple::StoreAvailability, apw_core::apple::ApiError> {
            self.result(store, parts).await
        }
        async fn pickup_message_nearby(
            &self,
            _region: &'static apw_core::model::Region,
            _location: &str,
            parts: &[String],
        ) -> Result<Vec<apw_core::apple::StoreAvailability>, apw_core::apple::ApiError> {
            self.result("R238", parts).await.map(|store| vec![store])
        }
    }
    fn manual_target() -> Target {
        Target {
            locale: "en_AU".into(),
            store_number: "R238".into(),
            store_title: "Sydney".into(),
            part_number: "SKU".into(),
            product_name: "Test phone".into(),
            companion_part: None,
            pickup_location: None,
        }
    }
    async fn api(app: &Arc<App>, method: &str, path: &str, payload: Value) -> (StatusCode, Value) {
        request(
            app.clone(),
            method,
            path,
            payload,
            Some("test-secret"),
            None,
        )
        .await
    }
    async fn wait_for_manual_end(app: &Arc<App>) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while app.manual_refreshing.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("manual refresh did not finish");
    }

    #[tokio::test]
    async fn manual_refresh_from_pause_is_one_round_and_has_a_30_second_floor() {
        let fetcher = ManualFetcher::new("");
        let (app, _dir) = test_app_with_fetcher(fetcher.clone()).await;
        app.replace_targets(vec![manual_target()]).await.unwrap();
        app.preferences.write().await.pushover_enabled = true;
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::OK
        );
        let (_, state) = api(&app, "GET", "/api/state", Value::Null).await;
        assert_eq!(state["running"], false);
        assert_eq!(state["refreshingStock"], true);
        assert!(state["nextManualCheckAt"].as_u64().unwrap() > now_ms());
        // Repeated requests join the same blocked fetch rather than cancelling
        // it or queueing another round.
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::OK
        );
        fetcher.permits.add_permits(1);
        wait_for_manual_end(&app).await;
        assert_eq!(fetcher.calls.load(Ordering::Acquire), 1);
        assert!(!app.watcher.is_running().await);
        let (_, state) = api(&app, "GET", "/api/state", Value::Null).await;
        assert_eq!(state["running"], false);
        assert_eq!(state["refreshingStock"], false);
        assert_eq!(state["targets"][0]["availability"]["kind"], "in_stock");
        assert!(state["targets"][0]["lastCheckedMs"].as_u64().is_some());
        assert!(
            app.db.history().unwrap().is_empty(),
            "Paused manual checks created notification jobs"
        );
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn manual_refresh_advances_running_schedule_and_coalesces_active_rounds() {
        let fetcher = ManualFetcher::new("");
        let (app, _dir) = test_app_with_fetcher(fetcher.clone()).await;
        app.replace_targets(vec![manual_target()]).await.unwrap();
        assert_eq!(
            api(&app, "PUT", "/api/settings", json!({"intervalSeconds":300}))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            api(&app, "POST", "/api/monitor", json!({"running":true}))
                .await
                .0,
            StatusCode::OK
        );
        let generation = app.generation.load(Ordering::Acquire);
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::OK
        );
        assert_eq!(
            app.generation.load(Ordering::Acquire),
            generation,
            "Joining an active round restarted it"
        );
        fetcher.permits.add_permits(1);
        wait_for_manual_end(&app).await;
        assert!(app.watcher.is_running().await);
        assert_eq!(fetcher.calls.load(Ordering::Acquire), 1);
        assert!(app.health.read().await.next_check_at.unwrap() > now_ms() + 120_000);
        // Advance only the 30s manual gate; the configured 300s wait remains.
        app.db.set("manualNotBefore", &0_u64).unwrap();
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::OK
        );
        fetcher.permits.add_permits(1);
        wait_for_manual_end(&app).await;
        assert_eq!(fetcher.calls.load(Ordering::Acquire), 2);
        assert!(app.watcher.is_running().await);
        assert_eq!(app.preferences.read().await.interval_seconds, 300);
        assert_eq!(
            api(&app, "GET", "/api/state", Value::Null).await.1["running"],
            true
        );
        app.watcher.stop().await;
    }

    #[tokio::test]
    async fn instant_manual_failure_finishes_and_cannot_bypass_real_cooldowns() {
        for failure in ["blocked", "rate_limited", "transport"] {
            let fetcher = ManualFetcher::new(failure);
            // Complete without an artificial network delay: this covers the
            // upstream client's immediate cached cooldown/error response.
            fetcher.permits.add_permits(1);
            let (app, _dir) = test_app_with_fetcher(fetcher.clone()).await;
            app.replace_targets(vec![manual_target()]).await.unwrap();
            assert_eq!(
                api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
                StatusCode::OK
            );
            wait_for_manual_end(&app).await;
            assert!(!app.watcher.is_running().await);
            app.db.set("manualNotBefore", &0_u64).unwrap();
            assert!(app.db.get::<u64>("stockCooldownUntil").unwrap().unwrap() > now_ms());
            assert_eq!(
                api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
                StatusCode::TOO_MANY_REQUESTS
            );
            assert_eq!(fetcher.calls.load(Ordering::Acquire), 1);
        }
    }

    #[tokio::test]
    async fn keychain_wait_cannot_hold_up_one_shot_completion() {
        let fetcher = ManualFetcher::new("");
        fetcher.permits.add_permits(1);
        let (app, _dir) = test_app_with_fetcher(fetcher.clone()).await;
        app.replace_targets(vec![manual_target()]).await.unwrap();
        let _credential_save = app.credential_operation.lock().await;
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::OK
        );
        wait_for_manual_end(&app).await;
        assert!(!app.watcher.is_running().await);
        assert_eq!(fetcher.calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn manual_refresh_does_not_release_previously_paused_notifications() {
        let fetcher = ManualFetcher::new("");
        let (app, _dir) = test_app_with_fetcher(fetcher.clone()).await;
        let target = manual_target();
        app.replace_targets(vec![target.clone()]).await.unwrap();
        let mut prefs = app.preferences.read().await.clone();
        prefs.pushover_enabled = true;
        *app.preferences.write().await = prefs.clone();
        app.db
            .observe(
                &[apw_core::watcher::TargetState {
                    target,
                    availability: Availability::InStock,
                    last_checked_ms: Some(now_ms()),
                    consecutive_failures: 0,
                }],
                &prefs,
            )
            .unwrap();
        assert_eq!(app.db.history().unwrap()[0].status, "pending");
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::OK
        );
        let deliveries = tokio::spawn(deliver_notifications(app.clone()));
        // An incorrect raw-watcher-running gate would attempt the job and
        // record a credential error. The empty fake credentials cannot send a
        // real notification even if this regression is reintroduced.
        tokio::time::sleep(Duration::from_millis(2_100)).await;
        assert_eq!(app.db.history().unwrap()[0].status, "pending");
        deliveries.abort();
        fetcher.permits.add_permits(1);
        wait_for_manual_end(&app).await;
    }

    #[tokio::test]
    async fn pause_cancels_manual_round_without_late_results() {
        let fetcher = ManualFetcher::new("");
        let (app, _dir) = test_app_with_fetcher(fetcher.clone()).await;
        app.replace_targets(vec![manual_target()]).await.unwrap();
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::OK
        );
        assert_eq!(
            api(&app, "POST", "/api/monitor", json!({"running":false}))
                .await
                .0,
            StatusCode::OK
        );
        fetcher.permits.add_permits(1);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!app.watcher.is_running().await);
        assert!(!app.manual_refreshing.load(Ordering::Acquire));
        assert!(!app.one_shot.load(Ordering::Acquire));
        assert!(
            app.observations
                .read()
                .await
                .row(&manual_target())
                .last_checked_ms
                .is_none()
        );
    }

    #[tokio::test]
    async fn polling_interval_accepts_30_and_rejects_29_seconds() {
        let (app, _dir) = test_app().await;
        assert_eq!(app.preferences.read().await.interval_seconds, 30);
        assert_eq!(
            api(&app, "PUT", "/api/settings", json!({"intervalSeconds":30}))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            api(&app, "PUT", "/api/settings", json!({"intervalSeconds":29}))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.db
                .get::<Preferences>("preferences")
                .unwrap()
                .unwrap()
                .interval_seconds,
            30
        );
        assert_eq!(
            api(&app, "POST", "/api/monitor/refresh", json!({})).await.0,
            StatusCode::BAD_REQUEST
        );
    }

    #[derive(Clone)]
    struct DelayedSecondFetcher {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        second_started: Arc<tokio::sync::Notify>,
        release_second: Arc<tokio::sync::Notify>,
    }
    impl DelayedSecondFetcher {
        async fn result(
            &self,
            store: &str,
            parts: &[String],
        ) -> Result<apw_core::apple::StoreAvailability, apw_core::apple::ApiError> {
            if self.calls.fetch_add(1, Ordering::AcqRel) > 0 {
                self.second_started.notify_one();
                self.release_second.notified().await;
            }
            Ok(apw_core::apple::StoreAvailability {
                store_number: store.to_owned(),
                store_name: store.to_owned(),
                parts: parts
                    .iter()
                    .map(|part| {
                        (
                            part.clone(),
                            apw_core::apple::PartStatus {
                                part_number: part.clone(),
                                availability: Availability::InStock,
                                product_title: None,
                                pickup_display: "available".into(),
                            },
                        )
                    })
                    .collect(),
            })
        }
    }
    impl apw_core::apple::Fetcher for DelayedSecondFetcher {
        async fn pickup_message(
            &self,
            _region: &'static apw_core::model::Region,
            store: &str,
            parts: &[String],
        ) -> Result<apw_core::apple::StoreAvailability, apw_core::apple::ApiError> {
            self.result(store, parts).await
        }
        async fn pickup_message_nearby(
            &self,
            _region: &'static apw_core::model::Region,
            location: &str,
            parts: &[String],
        ) -> Result<Vec<apw_core::apple::StoreAvailability>, apw_core::apple::ApiError> {
            let store = if location == "Sydney" { "R238" } else { "R180" };
            self.result(store, parts).await.map(|result| vec![result])
        }
    }

    #[tokio::test]
    async fn first_store_is_visible_and_alert_is_queued_before_slow_round_finishes() {
        let fetcher = DelayedSecondFetcher {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            second_started: Arc::new(tokio::sync::Notify::new()),
            release_second: Arc::new(tokio::sync::Notify::new()),
        };
        let (app, _dir) = test_app_with_fetcher(fetcher.clone()).await;
        // The nearby response omits Chatswood, so the core must perform its
        // exact-store fallback while Sydney is already visible in the API.
        let targets = ["R238", "R253"]
            .into_iter()
            .map(|number| Target {
                locale: "en_AU".into(),
                store_number: number.into(),
                store_title: number.into(),
                part_number: "SKU".into(),
                product_name: "Test iPhone".into(),
                companion_part: None,
                pickup_location: None,
            })
            .collect();
        app.replace_targets(targets).await.unwrap();
        app.preferences.write().await.pushover_enabled = true;
        app.watcher.start().await;
        tokio::time::timeout(Duration::from_secs(2), fetcher.second_started.notified())
            .await
            .unwrap();
        let (first_key, first_checked_at) = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (status, body) = request(
                    app.clone(),
                    "GET",
                    "/api/state",
                    Value::Null,
                    Some("test-secret"),
                    None,
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                let rows = body["targets"].as_array().unwrap();
                let available: Vec<_> = rows
                    .iter()
                    .filter(|row| row["availability"]["kind"] == "in_stock")
                    .collect();
                if available.len() == 1 && app.db.history().unwrap().len() == 1 {
                    assert!(
                        body["healthy"].is_null(),
                        "The whole round must still be blocked"
                    );
                    assert_eq!(body["notifications"][0]["status"], "pending");
                    assert_eq!(body["progress"]["completedStores"], 1);
                    let first = available[0];
                    break (
                        first["key"].as_str().unwrap().to_owned(),
                        first["lastCheckedMs"].as_u64().unwrap(),
                    );
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            app.watcher
                .snapshot()
                .await
                .iter()
                .all(|row| row.last_checked_ms.is_none()),
            "The upstream snapshot is still behind the full-round barrier"
        );
        assert_eq!(app.db.history().unwrap()[0].created_at, first_checked_at);
        tokio::time::sleep(Duration::from_millis(10)).await;
        fetcher.release_second.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (_, body) = request(
                    app.clone(),
                    "GET",
                    "/api/state",
                    Value::Null,
                    Some("test-secret"),
                    None,
                )
                .await;
                if body["healthy"] == true {
                    let first = body["targets"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|row| row["key"] == first_key)
                        .unwrap();
                    assert_eq!(
                        first["lastCheckedMs"].as_u64(),
                        Some(first_checked_at),
                        "End-of-round reconciliation must not retimestamp the first result"
                    );
                    assert_eq!(app.db.history().unwrap().len(), 2);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app.watcher.stop().await;
    }
    async fn request(
        app: Arc<App>,
        method: &str,
        path: &str,
        payload: Value,
        token: Option<&str>,
        origin: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8765")
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("x-session-token", token);
        }
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        let response = router(app)
            .oneshot(builder.body(Body::from(payload.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[test]
    fn notification_test_uses_trimmed_drafts_without_changing_saved_credentials() {
        let saved = Credentials {
            pushover_token: "a".repeat(30),
            pushover_user: "b".repeat(30),
            bark_url: "https://push.example.test/saved-key".into(),
        };
        let before = serde_json::to_value(&saved).unwrap();
        let input: TestNotification = serde_json::from_value(json!({
            "channel": "pushover",
            "pushoverToken": format!("  {}\n", "c".repeat(30)),
            "pushoverUser": "  \n",
            "barkUrl": "invalid unrelated draft"
        }))
        .unwrap();
        let resolved = input.credentials_for_test(&saved);
        assert_eq!(resolved.pushover_token, "c".repeat(30));
        assert_eq!(resolved.pushover_user, saved.pushover_user);
        assert_eq!(resolved.bark_url, saved.bark_url);
        assert!(resolved.validate().is_ok());
        assert_eq!(serde_json::to_value(&saved).unwrap(), before);

        let input: TestNotification = serde_json::from_value(json!({
            "channel": "bark",
            "barkUrl": " https://push.example.test/new-key \n",
            "pushoverToken": "invalid unrelated draft",
            "pushoverUser": "invalid unrelated draft"
        }))
        .unwrap();
        let resolved = input.credentials_for_test(&saved);
        assert_eq!(resolved.bark_url, "https://push.example.test/new-key");
        assert_eq!(resolved.pushover_token, saved.pushover_token);
        assert_eq!(resolved.pushover_user, saved.pushover_user);
        assert!(resolved.validate().is_ok());
        assert_eq!(serde_json::to_value(&saved).unwrap(), before);
    }

    #[test]
    fn notification_test_accepts_new_credentials_and_legacy_or_blank_drafts() {
        for channel in ["pushover", "bark"] {
            let input: TestNotification = serde_json::from_value(json!({
                "channel": channel,
                "pushoverToken": "a".repeat(30),
                "pushoverUser": "b".repeat(30),
                "barkUrl": "https://push.example.test/new-key"
            }))
            .unwrap();
            let saved = input.credentials_for_test(&Credentials::default());
            assert!(saved.validate().is_ok());
            if channel == "pushover" {
                assert_eq!(saved.pushover_token, "a".repeat(30));
                assert_eq!(saved.pushover_user, "b".repeat(30));
                assert!(saved.bark_url.is_empty());
            } else {
                assert_eq!(saved.bark_url, "https://push.example.test/new-key");
                assert!(saved.pushover_token.is_empty());
                assert!(saved.pushover_user.is_empty());
            }
            for payload in [
                json!({"channel": channel}),
                json!({"channel": channel, "pushoverToken": "", "pushoverUser": " \n", "barkUrl": "\t"}),
            ] {
                let input: TestNotification = serde_json::from_value(payload).unwrap();
                assert_eq!(
                    serde_json::to_value(input.credentials_for_test(&saved)).unwrap(),
                    serde_json::to_value(&saved).unwrap()
                );
            }
        }
    }

    #[tokio::test]
    async fn notification_test_endpoint_does_not_save_drafts_or_change_preferences() {
        let (app, _dir) = test_app().await;
        let saved = Credentials {
            pushover_token: "a".repeat(30),
            pushover_user: "b".repeat(30),
            bark_url: "https://push.example.test/saved-key".into(),
        };
        let saved_value = serde_json::to_value(&saved).unwrap();
        *app.credentials.write().await = saved;
        let preferences = serde_json::to_value(&*app.preferences.read().await).unwrap();
        app.db.set("preferences", &preferences).unwrap();
        // Invalid drafts are rejected before any network call. Saved values are
        // valid, so these errors also prove the endpoint uses the supplied edits.
        for (channel, payload, expected_error) in [
            (
                "pushover",
                json!({"channel":"pushover", "pushoverToken":"draft-token-secret"}),
                "Pushover application token must contain 30 letters or numbers.",
            ),
            (
                "bark",
                json!({"channel":"bark", "barkUrl":"http://example.test/draft-url-secret"}),
                "Bark",
            ),
        ] {
            let (status, response) = request(
                app.clone(),
                "POST",
                "/api/notifications/test",
                payload,
                Some("test-secret"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(response["error"].as_str().unwrap().contains(expected_error));
            assert!(!response.to_string().contains("secret"));
            assert_eq!(
                serde_json::to_value(&*app.credentials.read().await).unwrap(),
                saved_value
            );
            assert_eq!(
                serde_json::to_value(&*app.preferences.read().await).unwrap(),
                preferences
            );
            assert_eq!(
                app.db.get::<Value>("preferences").unwrap().unwrap(),
                preferences
            );
            let history = app.db.history().unwrap();
            assert!(
                history
                    .iter()
                    .any(|entry| entry.channel == channel && entry.status == "failed")
            );
            assert!(!serde_json::to_string(&history).unwrap().contains("secret"));
        }
    }

    #[tokio::test]
    async fn regions_and_catalogues_are_available_without_network_requests() {
        let (app, _dir) = test_app().await;
        let (status, listing) = request(
            app.clone(),
            "GET",
            "/api/regions",
            Value::Null,
            Some("test-secret"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["regions"].as_array().unwrap().len(), 25);
        assert!(
            listing["regions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["locale"] == "zh_CN" && r["baseUrl"] == "https://www.apple.com.cn")
        );
        for region in REGIONS {
            let snapshot = app.products.get(region.locale, &app.catalog).await.unwrap();
            assert!(
                !snapshot.products.is_empty(),
                "{} has no products",
                region.locale
            );
            assert!(
                !app.catalog.stores(region.locale).unwrap().is_empty(),
                "{} has no stores",
                region.locale
            );
        }
        let (_, australia) = request(
            app.clone(),
            "GET",
            "/api/catalog",
            Value::Null,
            Some("test-secret"),
            None,
        )
        .await;
        assert_eq!(australia["locale"], "en_AU");
        let (_, china) = request(
            app.clone(),
            "GET",
            "/api/catalog?locale=zh_CN",
            Value::Null,
            Some("test-secret"),
            None,
        )
        .await;
        assert_eq!(china["locale"], "zh_CN");
        assert_ne!(
            australia["products"][0]["partNumber"],
            china["products"][0]["partNumber"]
        );
        assert_eq!(
            request(
                app,
                "GET",
                "/api/catalog?locale=xx_XX",
                Value::Null,
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn mixed_region_watches_preserve_locale_and_validate_regional_catalogues() {
        let (app, _dir) = test_app().await;
        for locale in ["en_AU", "en_US"] {
            let product = app
                .products
                .get(locale, &app.catalog)
                .await
                .unwrap()
                .products[0]
                .part_number
                .clone();
            let store = app.catalog.stores(locale).unwrap()[0].number.clone();
            let (status, _) = request(
                app.clone(),
                "POST",
                "/api/targets",
                json!({"locale":locale,"storeNumbers":[store],"partNumbers":[product]}),
                Some("test-secret"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let saved = app.db.targets().unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|t| t.locale == "en_AU"));
        assert!(saved.iter().any(|t| t.locale == "en_US"));
        let wrong_store = app.catalog.stores("zh_CN").unwrap()[0].number.clone();
        let us_product = app
            .products
            .get("en_US", &app.catalog)
            .await
            .unwrap()
            .products[0]
            .part_number
            .clone();
        assert_eq!(
            request(
                app.clone(),
                "POST",
                "/api/targets",
                json!({"locale":"en_US","storeNumbers":[wrong_store],"partNumbers":[us_product]}),
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                app.clone(),
                "POST",
                "/api/targets",
                json!({"locale":"xx_XX","storeNumbers":["R238"],"partNumbers":["SKU"]}),
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(app.db.targets().unwrap().len(), 2);
    }

    #[test]
    fn distinct_part_budget_is_scoped_to_region() {
        let targets: Vec<Target> = ["en_AU", "en_US"]
            .into_iter()
            .flat_map(|locale| {
                (0..20).map(move |n| Target {
                    locale: locale.into(),
                    store_number: "R238".into(),
                    store_title: "Store".into(),
                    part_number: format!("{locale}-{n}"),
                    product_name: "Product".into(),
                    companion_part: None,
                    pickup_location: None,
                })
            })
            .collect();
        assert!(validate_targets(&targets).is_ok());
    }

    #[tokio::test]
    async fn retired_store_is_removed_and_cannot_be_readded() {
        let (app, _dir) = test_app().await;
        let (_, catalogue) = request(
            app.clone(),
            "GET",
            "/api/catalog",
            Value::Null,
            Some("test-secret"),
            None,
        )
        .await;
        assert!(
            !catalogue["stores"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["number"] == "R458")
        );
        let part = app
            .products
            .get("en_AU", &app.catalog)
            .await
            .unwrap()
            .products[0]
            .part_number
            .clone();
        let (status, _) = request(
            app.clone(),
            "POST",
            "/api/targets",
            json!({"storeNumbers":["R458"],"partNumbers":[part]}),
            Some("test-secret"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let make = |number: &str| Target {
            locale: "en_AU".into(),
            store_number: number.into(),
            store_title: number.into(),
            part_number: "SKU".into(),
            product_name: "Phone".into(),
            companion_part: None,
            pickup_location: None,
        };
        app.db.save_targets(&[make("R458"), make("R238")]).unwrap();
        let remaining = load_active_targets(&app.db).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].store_number, "R238");
        assert_eq!(app.db.targets().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn changing_language_preserves_other_preferences_and_watches() {
        let (app, _dir) = test_app().await;
        let prefs = Preferences {
            interval_seconds: 120,
            ..Default::default()
        };
        app.db.set("preferences", &prefs).unwrap();
        *app.preferences.write().await = prefs;
        let (status, _) = request(
            app.clone(),
            "PUT",
            "/api/language",
            json!({"language":"zh-Hans"}),
            Some("test-secret"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let stored = app.db.get::<Preferences>("preferences").unwrap().unwrap();
        assert_eq!(stored.interval_seconds, 120);
        assert_eq!(stored.notification_language, "zh-Hans");
        assert!(!stored.pushover_enabled);
        assert!(app.db.targets().unwrap().is_empty());
    }
    #[tokio::test]
    async fn api_requires_token_and_same_origin() {
        let (app, _dir) = test_app().await;
        assert_eq!(
            request(app.clone(), "GET", "/api/state", Value::Null, None, None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                app.clone(),
                "GET",
                "/api/state",
                Value::Null,
                Some("test-secret"),
                Some("https://malicious.example")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let (status, value) = request(
            app,
            "GET",
            "/api/state",
            Value::Null,
            Some("test-secret"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["running"], false);
        assert!(!value.to_string().contains("test-secret"));
    }
    #[tokio::test]
    async fn add_remove_targets_persists_and_updates_engine() {
        let (app, _dir) = test_app().await;
        let part = app
            .products
            .get("en_AU", &app.catalog)
            .await
            .unwrap()
            .products[0]
            .part_number
            .clone();
        let number = app.catalog.stores("en_AU").unwrap()[0].number.clone();
        let payload = json!({"storeNumbers":[number],"partNumbers":[part]});
        assert_eq!(
            request(
                app.clone(),
                "POST",
                "/api/targets",
                payload.clone(),
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            request(
                app.clone(),
                "POST",
                "/api/targets",
                payload,
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(app.db.targets().unwrap().len(), 1);
        assert_eq!(app.watcher.snapshot().await.len(), 1);
        let key = app.db.targets().unwrap()[0].key().to_string();
        assert_eq!(
            request(
                app.clone(),
                "POST",
                "/api/targets/remove",
                json!({"key":key}),
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(app.db.targets().unwrap().is_empty());
        assert!(app.watcher.snapshot().await.is_empty());
    }
    #[tokio::test]
    async fn bad_configuration_does_not_mutate_settings() {
        let (app, _dir) = test_app().await;
        assert_eq!(
            request(
                app.clone(),
                "PUT",
                "/api/settings",
                json!({"intervalSeconds":1}),
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                app.clone(),
                "PUT",
                "/api/settings",
                json!({"intervalSeconds":60,"pushoverEnabled":true}),
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(app.preferences.read().await.interval_seconds, 30);
        assert!(!app.preferences.read().await.pushover_enabled);
        assert_eq!(
            request(
                app,
                "POST",
                "/api/monitor",
                json!({"running":true}),
                Some("test-secret"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    #[test]
    fn batch_limit_includes_companion_parts() {
        let targets: Vec<Target> = (0..20)
            .map(|n| Target {
                locale: "en_AU".into(),
                store_number: "R238".into(),
                store_title: "Sydney".into(),
                part_number: format!("SKU{n}"),
                product_name: "Watch".into(),
                companion_part: Some("BAND".into()),
                pickup_location: None,
            })
            .collect();
        assert!(validate_targets(&targets).is_err());
        assert!(validate_targets(&targets[..19]).is_ok());
    }
    #[test]
    fn failures_are_never_translated_to_unavailable() {
        let message = availability_detail(&Availability::Unknown(UnknownReason::RateLimited));
        assert!(message.contains("limiting"));
        assert!(!message.contains("no pickup"));
    }
}
