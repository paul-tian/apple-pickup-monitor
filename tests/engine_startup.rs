use fs2::FileExt;
use serde_json::{Value, json};
use std::{fs::OpenOptions, process::Command};

#[test]
fn native_engine_explains_a_live_data_lock_without_replacing_it() {
    let data = tempfile::tempdir().unwrap();
    let lock_path = data.path().join("instance.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    lock.try_lock_exclusive().unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_apple-pickup-monitor"))
        .args(["--native", "--data-dir"])
        .arg(data.path())
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(1));
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        response,
        json!({"id": 0, "status": 409, "body": {"code": "instance_in_use"}})
    );
    assert!(result.stdout.ends_with(b"\n"));
    assert!(!data.path().join("monitor.sqlite").exists());
    assert!(!data.path().join("session.json").exists());
    // The original inode remains locked: failure must never replace the lock
    // file and accidentally allow a second engine to open the same database.
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert_eq!(
        contender.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
