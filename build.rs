use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=APPLE_PICKUP_APP_VERSION");

    // Xcode owns the app's marketing version. Standalone Cargo builds retain
    // the crate version, while changing this override invalidates Cargo's cache.
    let version = match env::var("APPLE_PICKUP_APP_VERSION") {
        Ok(version) => version,
        Err(env::VarError::NotPresent) => {
            env::var("CARGO_PKG_VERSION").expect("Cargo must provide its package version")
        }
        Err(env::VarError::NotUnicode(_)) => {
            panic!("APPLE_PICKUP_APP_VERSION must contain three numeric components")
        }
    };
    let components: Vec<_> = version.split('.').collect();
    assert!(
        components.len() == 3
            && components
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
        "APPLE_PICKUP_APP_VERSION must contain three numeric components (for example, 0.5.0)"
    );
    println!("cargo:rustc-env=APPLE_PICKUP_APP_VERSION={version}");
}
