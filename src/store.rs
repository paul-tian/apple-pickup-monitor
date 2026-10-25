use apw_core::{
    model::{Availability, Target, region_by_locale},
    watcher::TargetState,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const ALERT_TTL_MS: u64 = 15 * 60 * 1000;

fn stock_notification(target: &Target, language: &str) -> (&'static str, String) {
    let region = region_by_locale(&target.locale)
        .map(|r| r.title)
        .unwrap_or(&target.locale);
    let (title, instruction) = match language {
        "zh-Hans" => (
            "Apple 到店取货有货",
            "查询时可到店取货，请前往 Apple 官网确认并购买。",
        ),
        "zh-Hant" => (
            "Apple 到店取貨有貨",
            "查詢時可到店取貨，請前往 Apple 官網確認並購買。",
        ),
        _ => (
            "Apple pickup available",
            "Available for pickup when checked. Open Apple to confirm and purchase.",
        ),
    };
    (
        title,
        format!(
            "{}\n{} · {}\n{}",
            target.product_name, region, target.store_title, instruction
        ),
    )
}

pub fn localized_test_notification(language: &str) -> (&'static str, &'static str) {
    match language {
        "zh-Hans" => ("Apple 取货监控", "通知已连接。到店取货提醒将发送到此设备。"),
        "zh-Hant" => ("Apple 取貨監控", "通知已連線。到店取貨提醒將傳送到此裝置。"),
        _ => (
            "Apple Pickup Monitor",
            "Notifications are connected. Your pickup alerts will arrive here.",
        ),
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn sql_time(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    pub interval_seconds: u64,
    pub pushover_enabled: bool,
    pub bark_enabled: bool,
    pub notification_language: String,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            interval_seconds: 60,
            pushover_enabled: false,
            bark_enabled: false,
            notification_language: "en".into(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub id: i64,
    pub created_at: u64,
    pub title: String,
    pub message: String,
    pub channel: String,
    pub status: String,
    pub error: Option<String>,
    #[serde(skip)]
    pub attempts: u32,
    #[serde(skip)]
    pub url: Option<String>,
    #[serde(skip)]
    pub target_key: Option<String>,
}

pub struct Database {
    connection: Mutex<Connection>,
}
type Result<T> = std::result::Result<T, String>;
impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path).map_err(|e| e.to_string())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        connection
            .execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| e.to_string())?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS targets (
                key TEXT PRIMARY KEY, json TEXT NOT NULL, confirmed TEXT,
                added_at INTEGER NOT NULL DEFAULT 0, last_observed_at INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS deliveries (
                id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL, title TEXT NOT NULL,
                message TEXT NOT NULL, channel TEXT NOT NULL, status TEXT NOT NULL,
                error TEXT, attempts INTEGER NOT NULL DEFAULT 0, due INTEGER NOT NULL,
                expires INTEGER NOT NULL, url TEXT, target_key TEXT);",
        )
        .map_err(|e| e.to_string())?;
        // Upgrade existing prototype databases. Unidentified legacy queued
        // alerts are canceled by cleanup; completed history stays intact.
        for (table, column, statement) in [
            (
                "deliveries",
                "target_key",
                "ALTER TABLE deliveries ADD COLUMN target_key TEXT",
            ),
            (
                "targets",
                "added_at",
                "ALTER TABLE targets ADD COLUMN added_at INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "targets",
                "last_observed_at",
                "ALTER TABLE targets ADD COLUMN last_observed_at INTEGER NOT NULL DEFAULT 0",
            ),
        ] {
            if !has_column(&tx, table, column)? {
                tx.execute(statement, []).map_err(|e| e.to_string())?;
            }
        }
        cleanup_deliveries(&tx, now_ms())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        let value: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?", [key], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        value
            .map(|s| serde_json::from_str(&s).map_err(|e| format!("Stored {key} is invalid: {e}")))
            .transpose()
    }
    pub fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
        self.connection.lock().map_err(|_| "Database lock failed")?.execute(
            "INSERT INTO kv(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,json]
        ).map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn targets(&self) -> Result<Vec<Target>> {
        let conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        let mut stmt = conn
            .prepare("SELECT json FROM targets ORDER BY rowid")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        })
        .collect()
    }
    pub fn save_targets(&self, targets: &[Target]) -> Result<()> {
        let mut conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let keys: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT key FROM targets")
                .map_err(|e| e.to_string())?;
            stmt.query_map([], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?
        };
        for key in keys {
            if !targets.iter().any(|t| t.key().to_string() == key) {
                // Deleting and later re-adding a monitoring target cannot revive its alerts.
                cancel_target_deliveries(&tx, &key, "Monitoring target removed before delivery")?;
                tx.execute("DELETE FROM targets WHERE key=?", [&key])
                    .map_err(|e| e.to_string())?;
            }
        }
        let now = sql_time(now_ms());
        for target in targets {
            let json = serde_json::to_string(target).map_err(|e| e.to_string())?;
            tx.execute("INSERT INTO targets(key,json,added_at) VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET json=excluded.json", params![target.key().to_string(), json, now]).map_err(|e| e.to_string())?;
        }
        cleanup_deliveries(&tx, now_ms())?;
        tx.commit().map_err(|e| e.to_string())
    }
    /// Confirmed state and outbox updates commit together. Unknown observations
    /// never rearm an alert; confirmed unavailable observations cancel it.
    pub fn observe(&self, snapshot: &[TargetState], prefs: &Preferences) -> Result<()> {
        let mut conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        for state in snapshot {
            let Some(checked_at) = state.last_checked_ms else {
                continue;
            };
            // Use observation age, not insertion time. Do not turn an old or
            // future-dated snapshot into a fresh notification.
            if checked_at > now || now.saturating_sub(checked_at) >= ALERT_TTL_MS {
                continue;
            }
            let key = state.target.key().to_string();
            let previous: Option<(Option<String>, i64, i64)> = tx
                .query_row(
                    "SELECT confirmed,added_at,last_observed_at FROM targets WHERE key=?",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some((previous, added_at, last_observed_at)) = previous else {
                continue;
            };
            let checked_sql = sql_time(checked_at);
            if checked_sql < added_at || checked_sql < last_observed_at {
                continue;
            }
            tx.execute(
                "UPDATE targets SET last_observed_at=? WHERE key=?",
                params![checked_sql, key],
            )
            .map_err(|e| e.to_string())?;
            let confirmed = match state.availability {
                Availability::InStock => "in_stock",
                Availability::OutOfStock => "out_of_stock",
                Availability::Unknown(_) => continue,
            };
            if confirmed == "out_of_stock" {
                cancel_target_deliveries(&tx, &key, "Pickup became unavailable before delivery")?;
            } else if previous.as_deref() != Some("in_stock") {
                let (title, message) =
                    stock_notification(&state.target, &prefs.notification_language);
                let region = region_by_locale(&state.target.locale).ok_or_else(|| {
                    format!("Unsupported monitoring region: {}", state.target.locale)
                })?;
                let url = format!(
                    "{}/shop/product/{}",
                    region.base_url, state.target.part_number
                );
                for (channel, enabled) in [
                    ("pushover", prefs.pushover_enabled),
                    ("bark", prefs.bark_enabled),
                ] {
                    if enabled {
                        insert_delivery(
                            &tx,
                            NewDelivery {
                                target_key: Some(&key),
                                created_at: checked_at,
                                channel,
                                title,
                                message: &message,
                                url: Some(&url),
                                status: "pending",
                                error: None,
                            },
                        )?;
                    }
                }
            }
            tx.execute(
                "UPDATE targets SET confirmed=? WHERE key=?",
                params![confirmed, key],
            )
            .map_err(|e| e.to_string())?;
        }
        cleanup_deliveries(&tx, now)?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn record_test(
        &self,
        channel: &str,
        result: &std::result::Result<(), String>,
    ) -> Result<()> {
        let conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        insert_delivery(
            &conn,
            NewDelivery {
                target_key: None,
                created_at: now_ms(),
                channel,
                title: "Test notification",
                message: "Apple Pickup Monitor test notification",
                url: None,
                status: if result.is_ok() { "sent" } else { "failed" },
                error: result.as_ref().err().map(String::as_str),
            },
        )?;
        cleanup_deliveries(&conn, now_ms())
    }
    pub fn history(&self) -> Result<Vec<Delivery>> {
        let conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        cleanup_deliveries(&conn, now_ms())?;
        let mut stmt = conn.prepare("SELECT id,created_at,title,message,channel,status,error,attempts,url,target_key FROM deliveries ORDER BY id DESC LIMIT 50").map_err(|e| e.to_string())?;
        stmt.query_map([], delivery_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| e.to_string())
    }
    pub fn next_delivery(&self) -> Result<Option<Delivery>> {
        let conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        let now = now_ms();
        cleanup_deliveries(&conn, now)?;
        conn.query_row("SELECT id,created_at,title,message,channel,status,error,attempts,url,target_key FROM deliveries
            WHERE status IN ('pending','retrying') AND due<=? AND created_at<=? AND expires>? ORDER BY id LIMIT 1",
            params![sql_time(now),sql_time(now),sql_time(now)], delivery_row).optional().map_err(|e| e.to_string())
    }
    /// Recheck a fetched job immediately before sending. The worker must also
    /// serialize dispatch with monitoring-target edits to close the check-to-send window.
    pub fn is_delivery_current(&self, delivery: &Delivery) -> Result<bool> {
        let conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        let now = now_ms();
        cleanup_deliveries(&conn, now)?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM deliveries WHERE id=? AND target_key=? AND attempts=?
            AND status IN ('pending','retrying') AND due<=? AND created_at<=? AND expires>?)",
            params![
                delivery.id,
                delivery.target_key,
                i64::from(delivery.attempts),
                sql_time(now),
                sql_time(now),
                sql_time(now)
            ],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
    }
    pub fn finish_delivery(
        &self,
        delivery: &Delivery,
        result: &std::result::Result<(), String>,
    ) -> Result<()> {
        let mut conn = self.connection.lock().map_err(|_| "Database lock failed")?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        cleanup_deliveries(&tx, now)?;
        let attempts: Option<i64> = tx.query_row("SELECT attempts FROM deliveries WHERE id=? AND target_key=? AND attempts=? AND status IN ('pending','retrying')",
            params![delivery.id,delivery.target_key,i64::from(delivery.attempts)], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        // Late completions must not resurrect canceled, expired or sent rows.
        if let Some(attempts) = attempts {
            let attempts = attempts.saturating_add(1);
            let status = if result.is_ok() {
                "sent"
            } else if attempts >= 3 {
                "failed"
            } else {
                "retrying"
            };
            let delay = if attempts == 1 { 30_000 } else { 120_000 };
            tx.execute(
                "UPDATE deliveries SET status=?,attempts=?,error=?,due=? WHERE id=?",
                params![
                    status,
                    attempts,
                    result.as_ref().err(),
                    sql_time(now.saturating_add(delay)),
                    delivery.id
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    // Only internal constant table names are passed here.
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let columns = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    for result in columns {
        if result.map_err(|e| e.to_string())? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
fn cancel_target_deliveries(conn: &Connection, key: &str, reason: &str) -> Result<()> {
    conn.execute("UPDATE deliveries SET status='canceled',error=? WHERE target_key=? AND status IN ('pending','retrying')", params![reason,key]).map_err(|e| e.to_string())?;
    Ok(())
}
fn cleanup_deliveries(conn: &Connection, now: u64) -> Result<()> {
    conn.execute(
        "UPDATE deliveries SET status='expired',error='Alert expired before delivery'
        WHERE status IN ('pending','retrying') AND (expires<=? OR created_at<=?)",
        params![sql_time(now), sql_time(now.saturating_sub(ALERT_TTL_MS))],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("UPDATE deliveries SET status='canceled',error='Monitoring target is no longer eligible for this alert'
        WHERE status IN ('pending','retrying') AND (target_key IS NULL OR NOT EXISTS (
            SELECT 1 FROM targets WHERE targets.key=deliveries.target_key AND targets.confirmed='in_stock'))", []).map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM deliveries WHERE status IN ('sent','failed','expired','canceled') AND id NOT IN (SELECT id FROM deliveries ORDER BY id DESC LIMIT 500)", []).map_err(|e| e.to_string())?;
    Ok(())
}
struct NewDelivery<'a> {
    target_key: Option<&'a str>,
    created_at: u64,
    channel: &'a str,
    title: &'a str,
    message: &'a str,
    url: Option<&'a str>,
    status: &'a str,
    error: Option<&'a str>,
}
fn insert_delivery(conn: &Connection, delivery: NewDelivery<'_>) -> Result<()> {
    conn.execute("INSERT INTO deliveries(created_at,title,message,channel,status,error,due,expires,url,target_key) VALUES(?,?,?,?,?,?,?,?,?,?)",
        params![sql_time(delivery.created_at),delivery.title,delivery.message,delivery.channel,delivery.status,delivery.error,
            sql_time(now_ms()),sql_time(delivery.created_at.saturating_add(ALERT_TTL_MS)),delivery.url,delivery.target_key]).map_err(|e| e.to_string())?;
    Ok(())
}
fn delivery_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Delivery> {
    let created_at: i64 = r.get(1)?;
    let attempts: i64 = r.get(7)?;
    Ok(Delivery {
        id: r.get(0)?,
        created_at: created_at.try_into().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Integer,
                Box::new(e),
            )
        })?,
        title: r.get(2)?,
        message: r.get(3)?,
        channel: r.get(4)?,
        status: r.get(5)?,
        error: r.get(6)?,
        attempts: attempts.try_into().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Integer,
                Box::new(e),
            )
        })?,
        url: r.get(8)?,
        target_key: r.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_uses_the_watched_storefront_and_selected_language() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let mut t = target();
        t.locale = "zh_CN".into();
        t.part_number = "TESTCH/A".into();
        db.save_targets(&[t.clone()]).unwrap();
        db.observe(
            &[TargetState {
                target: t,
                availability: Availability::InStock,
                last_checked_ms: Some(now_ms()),
                consecutive_failures: 0,
            }],
            &Preferences {
                pushover_enabled: true,
                notification_language: "zh-Hans".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let alerts = db.history().unwrap();
        assert_eq!(alerts.len(), 1);
        assert!(
            alerts[0]
                .url
                .as_ref()
                .unwrap()
                .starts_with("https://www.apple.com.cn/shop/product/")
        );
        assert_eq!(alerts[0].title, "Apple 到店取货有货");
        assert!(alerts[0].message.contains("查询时可到店取货"));
    }
    use apw_core::model::UnknownReason;
    fn target() -> Target {
        Target {
            locale: "en_AU".into(),
            store_number: "R238".into(),
            store_title: "Sydney".into(),
            part_number: "TESTX/A".into(),
            product_name: "Test product".into(),
            companion_part: None,
            pickup_location: None,
        }
    }
    fn prefs() -> Preferences {
        Preferences {
            pushover_enabled: true,
            ..Default::default()
        }
    }
    fn observation(availability: Availability) -> TargetState {
        TargetState {
            target: target(),
            availability,
            last_checked_ms: Some(now_ms()),
            consecutive_failures: 0,
        }
    }
    fn database_with_watch() -> Database {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.save_targets(&[target()]).unwrap();
        db
    }
    #[test]
    fn unknown_and_restart_do_not_duplicate_stock_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let db = Database::open(&path).unwrap();
        db.save_targets(&[target()]).unwrap();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        db.observe(
            &[observation(Availability::Unknown(
                UnknownReason::RateLimited,
            ))],
            &prefs(),
        )
        .unwrap();
        drop(db);
        let db = Database::open(&path).unwrap();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        assert_eq!(db.history().unwrap().len(), 1);
        assert!(db.next_delivery().unwrap().is_some());
        db.observe(&[observation(Availability::OutOfStock)], &prefs())
            .unwrap();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        assert_eq!(db.history().unwrap().len(), 2);
    }
    #[test]
    fn invalid_stored_preferences_are_not_silently_reset() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.set("preferences", &"wrong shape").unwrap();
        assert!(db.get::<Preferences>("preferences").is_err());
    }
    #[test]
    fn delivery_failure_does_not_rearm_stock() {
        let db = database_with_watch();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        let job = db.next_delivery().unwrap().unwrap();
        db.finish_delivery(&job, &Err("network unavailable".into()))
            .unwrap();
        db.finish_delivery(&job, &Err("duplicate completion".into()))
            .unwrap();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        assert_eq!(db.history().unwrap().len(), 1);
        assert_eq!(db.history().unwrap()[0].status, "retrying");
        assert_eq!(db.history().unwrap()[0].attempts, 1);
        assert!(db.next_delivery().unwrap().is_none());
    }
    #[test]
    fn removing_and_readding_a_watch_never_revives_its_old_alert() {
        let db = database_with_watch();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        let old = db.next_delivery().unwrap().unwrap();
        db.save_targets(&[]).unwrap();
        db.save_targets(&[target()]).unwrap();
        assert!(!db.is_delivery_current(&old).unwrap());
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        let fresh = db.next_delivery().unwrap().unwrap();
        assert_ne!(fresh.id, old.id);
        assert!(db.is_delivery_current(&fresh).unwrap());
        db.finish_delivery(&old, &Ok(())).unwrap();
        let history = db.history().unwrap();
        assert_eq!(
            history.iter().find(|d| d.id == old.id).unwrap().status,
            "canceled"
        );
    }
    #[test]
    fn confirmed_unavailable_cancels_retries_and_late_completions() {
        let db = database_with_watch();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        let job = db.next_delivery().unwrap().unwrap();
        db.finish_delivery(&job, &Err("network unavailable".into()))
            .unwrap();
        db.observe(&[observation(Availability::OutOfStock)], &prefs())
            .unwrap();
        assert!(!db.is_delivery_current(&job).unwrap());
        db.finish_delivery(&job, &Err("late failure".into()))
            .unwrap();
        assert_eq!(db.history().unwrap()[0].status, "canceled");
        assert!(db.next_delivery().unwrap().is_none());
    }
    #[test]
    fn fetched_alert_expiring_before_dispatch_is_not_sendable_or_revived() {
        let db = database_with_watch();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        let job = db.next_delivery().unwrap().unwrap();
        let expired = now_ms() - ALERT_TTL_MS - 1;
        db.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE deliveries SET created_at=?,expires=? WHERE id=?",
                params![sql_time(expired), sql_time(expired + ALERT_TTL_MS), job.id],
            )
            .unwrap();
        assert!(!db.is_delivery_current(&job).unwrap());
        db.finish_delivery(&job, &Ok(())).unwrap();
        assert_eq!(db.history().unwrap()[0].status, "expired");
        assert!(db.next_delivery().unwrap().is_none());
    }
    #[test]
    fn alert_age_is_measured_from_the_observation_not_enqueue_time() {
        let db = database_with_watch();
        db.connection
            .lock()
            .unwrap()
            .execute("UPDATE targets SET added_at=0", [])
            .unwrap();
        let checked = now_ms() - 60_000;
        let mut snapshot = observation(Availability::InStock);
        snapshot.last_checked_ms = Some(checked);
        db.observe(&[snapshot], &prefs()).unwrap();
        let job = db.next_delivery().unwrap().unwrap();
        assert_eq!(job.created_at, checked);
        let expiry: i64 = db
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT expires FROM deliveries WHERE id=?", [job.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(expiry, sql_time(checked + ALERT_TTL_MS));
    }
    #[test]
    fn expired_future_and_pre_add_observations_cannot_enqueue_alerts() {
        let db = database_with_watch();
        for checked in [
            now_ms() - ALERT_TTL_MS - 1,
            now_ms() + 60_000,
            now_ms() - 60_000,
        ] {
            let mut snapshot = observation(Availability::InStock);
            snapshot.last_checked_ms = Some(checked);
            db.observe(&[snapshot], &prefs()).unwrap();
        }
        assert!(db.history().unwrap().is_empty());
    }
    #[test]
    fn out_of_order_observations_do_not_undo_a_newer_unavailable_result() {
        let db = database_with_watch();
        db.connection
            .lock()
            .unwrap()
            .execute("UPDATE targets SET added_at=0", [])
            .unwrap();
        let now = now_ms();
        for (availability, checked) in [
            (Availability::InStock, now - 200),
            (Availability::OutOfStock, now - 100),
            (Availability::InStock, now - 150),
        ] {
            let mut snapshot = observation(availability);
            snapshot.last_checked_ms = Some(checked);
            db.observe(&[snapshot], &prefs()).unwrap();
        }
        assert_eq!(db.history().unwrap().len(), 1);
        assert_eq!(db.history().unwrap()[0].status, "canceled");
        assert!(db.next_delivery().unwrap().is_none());
    }
    #[test]
    fn duplicate_http_completion_cannot_requeue_a_sent_alert() {
        let db = database_with_watch();
        db.observe(&[observation(Availability::InStock)], &prefs())
            .unwrap();
        let job = db.next_delivery().unwrap().unwrap();
        db.finish_delivery(&job, &Ok(())).unwrap();
        db.finish_delivery(&job, &Err("late duplicate failure".into()))
            .unwrap();
        assert_eq!(db.history().unwrap()[0].status, "sent");
        assert_eq!(db.history().unwrap()[0].attempts, 1);
    }
    #[test]
    fn legacy_outbox_migrates_without_sending_unidentified_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE targets (key TEXT PRIMARY KEY,json TEXT NOT NULL,confirmed TEXT);
            CREATE TABLE deliveries (id INTEGER PRIMARY KEY,created_at INTEGER NOT NULL,title TEXT NOT NULL,
            message TEXT NOT NULL,channel TEXT NOT NULL,status TEXT NOT NULL,error TEXT,attempts INTEGER NOT NULL DEFAULT 0,
            due INTEGER NOT NULL,expires INTEGER NOT NULL,url TEXT);").unwrap();
        let now = sql_time(now_ms());
        for status in ["pending", "sent"] {
            conn.execute("INSERT INTO deliveries(created_at,title,message,channel,status,due,expires) VALUES(?,'Title','Message','pushover',?,?,?)",params![now,status,now,now+ALERT_TTL_MS as i64]).unwrap();
        }
        drop(conn);
        let db = Database::open(&path).unwrap();
        assert!(db.next_delivery().unwrap().is_none());
        let history = db.history().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].status, "sent");
        assert_eq!(history[1].status, "canceled");
        drop(db);
        assert_eq!(Database::open(&path).unwrap().history().unwrap().len(), 2);
    }
}
