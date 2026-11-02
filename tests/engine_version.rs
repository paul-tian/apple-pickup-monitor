use std::process::Command;

#[test]
fn version_query_does_not_initialize_application_data() {
    let root = tempfile::tempdir().unwrap();
    for flag in ["--version", "-V"] {
        let data = root.path().join("must-not-be-created");
        let result = Command::new(env!("CARGO_BIN_EXE_apple-pickup-monitor"))
            .arg("--data-dir")
            .arg(&data)
            .arg(flag)
            .output()
            .unwrap();

        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            format!("{}\n", env!("APPLE_PICKUP_APP_VERSION"))
        );
        assert!(result.stderr.is_empty());
        assert!(!data.exists());
    }
}
