mod catalogs;
mod ipc;
mod notifications;
mod observations;
mod progress;
mod service;
mod store;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    hash::{Hash, Hasher},
    io::Write,
    path::PathBuf,
    sync::Arc,
};

#[derive(Serialize, Deserialize)]
struct Session {
    port: u16,
    token: String,
}
struct Paths {
    data: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
}
fn private_dir(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("Could not create {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn private_file(path: &std::path::Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| e.to_string())
}
fn launch_url(session: &Session) -> String {
    // Reload an older browser tab on every launch; keep credentials out of HTTP URLs.
    format!(
        "http://127.0.0.1:{}/?launch={}#token={}",
        session.port,
        store::now_ms(),
        session.token
    )
}
fn open_browser(session: &Session) {
    let url = launch_url(session);
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
    #[cfg(not(target_os = "macos"))]
    println!("Open {url}");
}
#[tokio::main]
async fn main() {
    let native = std::env::args().any(|argument| argument == "--native");
    let status = match run().await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Apple Pickup Monitor: {error}");
            1
        }
    };
    if native || status != 0 {
        // Tokio's stdin and OS Keychain operations can own blocking threads.
        // The parent pipe is the lifetime authority; do not wait indefinitely
        // for an abandoned Keychain prompt after the parent has gone away.
        std::process::exit(status);
    }
}
async fn run() -> Result<(), String> {
    let mut custom_data = None;
    let mut port = 8765u16;
    let mut no_open = false;
    let mut native = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                custom_data = Some(PathBuf::from(args.next().ok_or("--data-dir needs a path")?))
            }
            "--port" => {
                port = args
                    .next()
                    .ok_or("--port needs a number")?
                    .parse()
                    .map_err(|_| "Invalid port")?
            }
            "--no-open" => no_open = true,
            "--native" => {
                native = true;
                no_open = true;
            }
            "--help" | "-h" => {
                println!(
                    "Apple Pickup Monitor\n  --no-open       Do not open the diagnostic browser\n  --native        Private stdin/stdout engine owned by the native app; no web server\n  --port NUMBER   Diagnostic web port (default 8765; 0 chooses a free port)\n  --data-dir PATH Isolated application data, cache, logs and Keychain namespace"
                );
                return Ok(());
            }
            value if value.starts_with("-psn_") => {}
            _ => return Err(format!("Unknown argument: {arg}")),
        }
    }
    let isolated = custom_data.is_some();
    let paths = if let Some(data) = custom_data {
        Paths {
            cache: data.join("cache"),
            logs: data.join("logs"),
            data,
        }
    } else {
        let home = dirs::home_dir().ok_or("Could not find your home directory")?;
        Paths {
            data: home.join("Library/Application Support/Apple Pickup Monitor"),
            cache: home.join("Library/Caches/Apple Pickup Monitor"),
            logs: home.join("Library/Logs/Apple Pickup Monitor"),
        }
    };
    for path in [&paths.data, &paths.cache, &paths.logs] {
        private_dir(path)?;
    }
    let lock = private_file(&paths.data.join("instance.lock"))?;
    let session_path = paths.data.join("session.json");
    if lock.try_lock_exclusive().is_err() {
        if native {
            return Err("Another monitoring instance is already using this application data. Quit that instance before opening this app.".into());
        }
        let session: Session = serde_json::from_slice(
            &std::fs::read(&session_path)
                .map_err(|_| "Another instance is starting. Try launching again shortly.")?,
        )
        .map_err(|_| "Another instance is starting. Try launching again shortly.")?;
        if !no_open {
            open_browser(&session);
        } else if !native {
            println!("Open {}", launch_url(&session));
        }
        println!("Already running at http://127.0.0.1:{}", session.port);
        return Ok(());
    }
    let listener = if native {
        // An older web-based build may have left this behind after a crash.
        // Native mode neither discovers nor publishes a network session.
        match std::fs::remove_file(&session_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not remove the obsolete web session: {error}"
                ));
            }
        }
        None
    } else {
        Some(
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => listener,
                Err(_) if port == 8765 => tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .map_err(|error| error.to_string())?,
                Err(error) => return Err(error.to_string()),
            },
        )
    };
    let origin = if let Some(listener) = &listener {
        port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        format!("http://127.0.0.1:{port}")
    } else {
        "http://native.invalid".to_string()
    };
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let db_path = paths.data.join("monitor.sqlite");
    let _ = private_file(&db_path)?;
    let db = Arc::new(store::Database::open(&db_path)?);
    let namespace = if isolated {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::fs::canonicalize(&paths.data)
            .map_err(|e| e.to_string())?
            .hash(&mut hasher);
        format!(
            "com.paultian.apple-pickup-monitor.test.{:x}",
            hasher.finish()
        )
    } else {
        "com.paultian.apple-pickup-monitor".into()
    };
    let (quit, mut quit_rx) = tokio::sync::watch::channel(false);
    let create_app = service::App::new(
        db,
        paths.cache.clone(),
        paths.logs.join("monitor.log"),
        &namespace,
        token.clone(),
        origin,
        quit,
    );
    if native {
        let result = ipc::serve(
            create_app,
            tokio::io::stdin(),
            tokio::io::stdout(),
            shutdown_signal(),
        )
        .await;
        drop(lock);
        return result;
    }
    let app = create_app.await?;
    let listener = listener.expect("diagnostic mode created a TCP listener");
    let session = Session { port, token };
    let mut session_file = private_file(&session_path)?;
    session_file.set_len(0).map_err(|e| e.to_string())?;
    session_file
        .write_all(&serde_json::to_vec(&session).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    session_file.sync_all().map_err(|e| e.to_string())?;
    println!("Apple Pickup Monitor ready at http://127.0.0.1:{port}");
    println!("Application data: {}", paths.data.display());
    if !no_open {
        open_browser(&session);
    } else if !native {
        println!("Open {}", launch_url(&session));
    }
    let stop_app = app.clone();
    let result = axum::serve(listener, service::router(app))
        .with_graceful_shutdown(async move {
            tokio::select! {_=shutdown_signal()=>{},_=quit_rx.changed()=>{}}
            stop_app.watcher.stop().await;
            let _ = stop_app.quit.send(true);
        })
        .await;
    let _ = std::fs::remove_file(session_path);
    drop(lock);
    result.map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
