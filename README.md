# Apple Pickup Monitor

A native macOS app for monitoring Apple Store pickup availability across 25 supported regions, with Pushover and Bark notifications. The SwiftUI interface follows the compact desktop layout of [ENCHIGO/apple-pickup-watcher](https://github.com/ENCHIGO/apple-pickup-watcher): product and store selection, collapsible settings, live stock table, and recent notifications. Its bundled Rust helper reuses that project's monitoring core.

Open the app to see your monitoring list. The app starts and owns its bundled monitoring engine automatically. Closing the window leaves monitoring active; reopen it from the menu bar or Dock. Quit Apple Pickup Monitor to stop the app and engine.

The native app communicates with its engine through private parent/child pipes. It does not start a TCP web server, discover a server through a session file, or require a connection/reconnection action. A fresh app launch starts paused. If the engine exits unexpectedly while the app remains open, the app attempts automatic recovery with bounded retry delays and resumes the requested monitoring state while respecting request cooldowns. If recovery cannot finish, the app reports the failure. The engine stops when its app-owned input closes, including when the app exits unexpectedly.

## Current scope

- The 25 storefront regions supported by the pinned upstream core; iPhone, iPad, Mac and Apple Watch products from each region's catalogue.
- Mixed-region monitoring lists. Selecting a different region changes the store/product picker without removing existing monitoring targets.
- English and Simplified Chinese interface options, independent of the monitored store region.
- Up to 100 store/product combinations and 20 distinct part numbers per region, including required Apple Watch bands.
- Shared nearby queries, immediate per-response updates, and explicit available/unavailable/unknown states.
- Persistent monitoring list and notification history; credentials stored in macOS Keychain.
- Pushover and Bark notifications with test actions.
- Hornsby (R458) is excluded because it permanently closed; older monitoring targets for that store are removed on startup.

The 60-second default is a minimum round interval, not a promise to check every store each minute. Requests are paced, and the app displays progress and the actual next check. Catalogue refreshes are limited to one per five minutes. After an Apple block, restarting conservatively preserves a wait of up to 30 minutes instead of bypassing the upstream cooldown.

This remains a development build until the signed distribution workflow is completed. The earlier service prototype passed live Apple Australia checks on 26 October 2026. These were point-in-time integration tests, not a guarantee of current stock or future endpoint availability. Real notification delivery needs to be verified with your own credentials.

## Regions and interface language

Supported regions are Australia, Austria, Belgium, Canada, mainland China, France, Germany, Hong Kong, India, Italy, Japan, Malaysia, Mexico, the Netherlands, Singapore, South Korea, Spain, Sweden, Switzerland, Taiwan, Thailand, Türkiye, the United Arab Emirates, the United Kingdom, and the United States. This is the upstream core's supported list, not a claim of coverage for every country with an Apple Store.

Use **Region** when adding monitoring targets to choose the Apple storefront, its products and its stores. Product names and colours come from that storefront and may remain in its local language. SKU suffixes differ between regions; the app checks each SKU against its own regional catalogue. Saved monitoring targets retain their region even when you change the picker.

Use **Interface language** in settings for **System**, **English**, or **简体中文**. The interface changes without changing any monitoring target's region. System uses a supported language matching your macOS preferences, with English as the fallback. Localized resources are packaged as standard `.lproj/Localizable.strings` files, so additional interface languages can be added independently of inventory-region support. Provider-generated error text may remain in the provider's language.

New pickup notifications use the chosen interface language. Their Apple purchase links point to the monitored region's storefront. Existing notification history retains the language it was originally sent in.

Australia and the United States have been tested against the live Apple endpoint in this project. The US smoke test on 26 October 2026 returned confirmed pickup availability at Apple Fifth Avenue. Other regional catalogues and request rules come from the upstream core and are covered by local validation, but each region still needs live verification on the network where the app will run.

## Build and run in Xcode

Start with a Mac, Xcode (including its first-launch components), and internet access. No Homebrew or existing Rust/CMake installation is required. The native target uses Xcode's recommended minimum macOS version, currently macOS 14.

After cloning, run this once from the repository folder:

```sh
./scripts/setup-build-tools.sh
```

The script installs Rust 1.99.1 (managed by rustup 1.29.2) and CMake 4.4.4 inside the ignored `.tools/` directory. Rust includes both `aarch64-apple-darwin` and `x86_64-apple-darwin` standard libraries, plus rustfmt and Clippy for the development checks. CMake's official universal binary supports Apple Silicon and Intel. Xcode supplies the C/Clang compiler used to build the helper's TLS dependency.

Installer/archive versions and SHA-256 hashes are pinned in `scripts/build-tools.env`. Downloads come from [Rust's official distribution server](https://rust-lang.github.io/rustup/installation/other.html) and [Kitware's CMake releases](https://cmake.org/download/); the script checks those hashes before executing or extracting them, and rustup checks its component downloads. Re-running setup reuses completed installations and repairs missing components. A failed download can be retried by running the same command.

Setup requires no administrator privileges, changes no shell profiles or system settings, and does not install into `/Applications` or your global Cargo/Rustup directories. Everything it installs or caches stays under `.tools/cargo`, `.tools/rustup`, `.tools/cmake`, and `.tools/downloads`. Xcode finds these tools automatically; there is no need to source an environment file or restart your shell. The tools are not copied into the finished app.

1. Open `ApplePickupMonitor.xcodeproj`.
2. Select the **Apple Pickup Monitor** scheme and **My Mac**.
3. Select your development team in Signing settings, or **Sign to Run Locally** for an ad-hoc build.
4. Build and run. The first build downloads the pinned upstream core and Cargo dependencies.

The project's **Build and embed monitoring helper** phase compiles the pinned Rust core and places the executable inside the app bundle. It checks requested Rust targets and CMake before compilation; missing build tools produce setup instructions instead of installing tools during an Xcode build.

For developers using their own tools, Cargo selection is `APPLE_PICKUP_CARGO` when set, then the project-local installation, then Cargo on `PATH` or in the standard Cargo home (normally `~/.cargo/bin`). An explicit `RUSTC` (or `CARGO_BUILD_RUSTC`) is respected; otherwise Cargo is paired with the compiler installed beside it. CMake selection is an explicit `CMAKE` path, then `.tools/cmake/CMake.app/Contents/bin/cmake`, then an existing installation on `PATH` or in `/Applications/CMake.app`. A custom toolchain needs Rust 1.98+ and CMake 3.22+. `./scripts/setup-rust-targets.sh` can install both macOS targets into an existing rustup-managed toolchain; it never installs into a different toolchain's home. No developer-specific helper path is used at runtime.

For a packaged local build:

```sh
./scripts/build-app.sh
```

The result is `dist/Apple Pickup Monitor.app`. The script uses `xcodebuild`, embeds the helper, checks runtime library paths, and preserves Xcode's signatures. It honors the project's signing settings unless `SIGNING_IDENTITY` is supplied. The default packaged build matches the current Mac. For an Intel build on Apple Silicon, set `APPLE_PICKUP_TARGET=x86_64-apple-darwin`; the setup script already installs its Rust target. Xcode Release/Archive can request both architectures; the helper phase builds each requested target and combines them with `lipo`. Cross-architecture builds need separate validation before release.

The Xcode target uses Apple Development signing for local builds. Select your development team in Signing settings. The helper uses the fixed signing identifier `com.paultian.apple-pickup-monitor.engine` so certificate-signed rebuilds retain its Keychain identity. `SIGNING_IDENTITY=- ./scripts/build-app.sh` explicitly creates an ad-hoc build when no signing certificate is available, but rebuilt ad-hoc helpers can prompt again for Keychain access. Switching from an older build or between development and Developer ID signing may require one new authorization for existing credentials. Use the release script below for Developer ID distribution. User script sandboxing is disabled specifically so Cargo can compile and package the helper in its build phase; App Sandbox is not enabled.

## Application data

The app bundle remains unchanged at runtime. Existing data from the web prototype is reused:

| Data | Location |
| --- | --- |
| Settings, monitoring list and SQLite notification history | `~/Library/Application Support/Apple Pickup Monitor/` |
| Refreshed catalogue cache | `~/Library/Caches/Apple Pickup Monitor/` |
| Logs | `~/Library/Logs/Apple Pickup Monitor/` |
| Notification credentials | macOS Keychain |

The helper's `--data-dir PATH` option isolates its files and Keychain namespace for testing. Native mode does not create a `session.json` file or expose a local listening port. Do not commit keys, local databases, logs or toolchain downloads.

## Notifications

Pushover requires an application API token and user/group key. Bark uses the push address supplied by its app or your Bark server. Enter these in the native settings panel and send a test notification before saving. Tests use the entered values, falling back to saved credentials for blank fields, without storing changes. Click Save settings to store credentials in Keychain and apply the notification preferences.

Alerts are triggered by newly observed availability. Confirmed stock state survives restarts, and a temporary unknown result does not rearm an alert. A confirmed unavailable result rearms the next restock alert. The durable outbox makes at most three delivery attempts and expires an alert 15 minutes after its observation. Removing a monitoring target or confirming it unavailable cancels pending alerts. Pausing monitoring also pauses the outbox.

A timeout after a provider accepts a message can still cause a duplicate on retry; the providers do not offer a shared exactly-once guarantee. Notification failures never alter the stock result. The app does not reserve stock or make purchases.

## Development checks

```sh
./scripts/cargo.sh test --locked
./scripts/cargo.sh clippy --locked --all-targets -- -D warnings
./scripts/test-native-models.sh
./scripts/test-native-transport.sh
./scripts/test-engine-lifecycle.sh
python3 scripts/tests/test_rust_toolchain.py
python3 scripts/tests/test_build_tools_setup.py
```

The native v0.4.0 build preserves the existing 16-target Australian database. Xcode build, 48 Rust tests, native model/localization tests, private-pipe transport tests, notification draft tests, automatic-recovery lifecycle tests, and bundle signature verification pass. Notification tests cover testing before saving, blank-field fallback, provider isolation, and preserving saved credentials after failures. A live check completed all 16 targets without a TCP listener or session file; Quit released the engine's data lock. Transport tests also verify that abrupt parent exit closes the child's input and ends the engine. The local build uses Apple Development signing for both the app and helper. Developer ID notarization and real Pushover/Bark delivery require the release credentials and notification keys respectively.

The standalone diagnostic web service can still be run manually, separately from the native app:

```sh
./scripts/cargo.sh run -- --no-open --data-dir /tmp/apple-pickup-monitor-test
```

Open the authenticated local address it prints. This optional mode is separate from the native app and must use its own `--data-dir` when both run at once; the same database cannot be owned by two engines. Normal app launch uses only the private pipe protocol. Rebuild the app after changing Rust or embedded web assets.

## Signed distribution

Use a **Developer ID Application** identity from an Apple Developer Program account. Keep its private key in Keychain or protected CI storage:

```sh
SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)" ./scripts/build-app.sh
```

The helper is signed before the containing app, with hardened runtime and a secure timestamp. Building does not submit software to Apple.

After configuring a `notarytool` credential profile in Keychain, explicitly notarize:

```sh
./scripts/notarize-app.sh YOUR_KEYCHAIN_PROFILE
```

This uploads the signed app, waits for acceptance, staples the ticket, verifies it, and writes `dist/Apple Pickup Monitor-notarized.zip`. Validate the downloaded archive on a clean Mac and test upgrades before publishing. `APP_VERSION` and `BUILD_NUMBER` override bundle version metadata. Supply the corresponding GPL source and dependency notices with a release.

## Apple endpoint behavior

The upstream core uses Apple's retail pickup endpoint. This is not a supported public inventory API, so response formats and regional behaviour can change. A working Apple website in a browser does not prove an automated request succeeded. Failed or incomplete queries remain unknown, never unavailable.

## License and attribution

GPL-3.0-or-later; see [LICENSE](LICENSE) and [NOTICE](NOTICE). The pinned upstream core revision is `842818f04828b792ce2d6a01c78847fe0e14766a`. Its predecessor is [hteen/apple-store-helper](https://github.com/hteen/apple-store-helper). The native UI adopts the upstream desktop organisation while using macOS controls. This project is not affiliated with or endorsed by Apple.
