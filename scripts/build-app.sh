#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"
if [[ "$(uname -s)" != Darwin ]]; then
  printf '%s\n' 'The macOS app must be built on macOS.' >&2
  exit 1
fi

case "${APPLE_PICKUP_TARGET:-$(uname -m)}" in
  arm64|aarch64-apple-darwin) BUILD_ARCH=arm64 ;;
  x86_64|x86_64-apple-darwin) BUILD_ARCH=x86_64 ;;
  *) printf '%s\n' 'APPLE_PICKUP_TARGET must be a macOS Rust target.' >&2; exit 1 ;;
esac
APP_VERSION="${APP_VERSION:-0.4.0}"
BUILD_NUMBER="${BUILD_NUMBER:-1}"
if [[ ! "$APP_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || [[ ! "$BUILD_NUMBER" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  printf '%s\n' 'APP_VERSION must be three numeric components; BUILD_NUMBER must have one to three.' >&2
  exit 1
fi

DERIVED_DATA="$PROJECT_ROOT/target/xcode"
export CLANG_MODULE_CACHE_PATH="$PROJECT_ROOT/target/ModuleCache.noindex"
mkdir -p "$CLANG_MODULE_CACHE_PATH"
XCODE_SETTINGS=(
  "ARCHS=$BUILD_ARCH" ONLY_ACTIVE_ARCH=YES
  "CLANG_MODULE_CACHE_PATH=$CLANG_MODULE_CACHE_PATH" "MODULE_CACHE_DIR=$CLANG_MODULE_CACHE_PATH"
  "MARKETING_VERSION=$APP_VERSION" "CURRENT_PROJECT_VERSION=$BUILD_NUMBER"
  ENABLE_HARDENED_RUNTIME=YES
)
# Honor the project's signing identity unless an explicit override is supplied.
# Silently replacing a certificate signature with ad-hoc signing breaks
# Keychain recognition whenever the helper is rebuilt.
if [[ -n "${SIGNING_IDENTITY:-}" ]]; then
  XCODE_SETTINGS+=("CODE_SIGN_IDENTITY=$SIGNING_IDENTITY")
fi
/usr/bin/xcodebuild \
  -project "$PROJECT_ROOT/ApplePickupMonitor.xcodeproj" \
  -scheme 'Apple Pickup Monitor' \
  -configuration Release \
  -derivedDataPath "$DERIVED_DATA" \
  -destination "platform=macOS,arch=$BUILD_ARCH" \
  "${XCODE_SETTINGS[@]}" build

BUILT_APP="$DERIVED_DATA/Build/Products/Release/Apple Pickup Monitor.app"
if [[ ! -x "$BUILT_APP/Contents/MacOS/ApplePickupMonitor" || ! -x "$BUILT_APP/Contents/Helpers/apple-pickup-monitor" ]]; then
  printf '%s\n' 'Xcode did not produce both the native app and its bundled monitoring helper.' >&2
  exit 1
fi

# The bundle must run without Homebrew, Cargo or other developer libraries.
for executable in "$BUILT_APP/Contents/MacOS/ApplePickupMonitor" "$BUILT_APP/Contents/Helpers/apple-pickup-monitor"; do
  while IFS= read -r library; do
    case "$library" in
      /System/Library/*|/usr/lib/*) ;;
      @rpath/*)
        if [[ ! -f "$BUILT_APP/Contents/Frameworks/${library#@rpath/}" ]]; then
          printf 'Unresolved bundled library: %s\n' "$library" >&2; exit 1
        fi ;;
      @executable_path/../Frameworks/*)
        if [[ ! -f "$BUILT_APP/Contents/Frameworks/${library#@executable_path/../Frameworks/}" ]]; then
          printf 'Unresolved bundled library: %s\n' "$library" >&2; exit 1
        fi ;;
      *) printf 'Unbundled runtime dependency: %s\n' "$library" >&2; exit 1 ;;
    esac
  done < <(/usr/bin/otool -L "$executable" | /usr/bin/awk 'NR > 1 { print $1 }')
done

mkdir -p "$PROJECT_ROOT/dist"
STAGING_ROOT="$(mktemp -d "$PROJECT_ROOT/dist/.app-build.XXXXXX")"
trap 'rm -rf "$STAGING_ROOT"' EXIT
APP="$STAGING_ROOT/Apple Pickup Monitor.app"
/usr/bin/ditto "$BUILT_APP" "$APP"
# ditto preserves the helper and app signatures produced by Xcode. Re-signing
# here would risk changing their identities, entitlements or runtime flags.
SIGNATURE="$(/usr/bin/codesign --display --verbose=4 "$APP" 2>&1)"
if [[ "$SIGNATURE" == *'Signature=adhoc'* ]]; then
  printf '%s\n' 'Ad-hoc signed for local testing. Rebuilt helpers may prompt for Keychain access again. This is not a distribution release.'
else
  printf '%s\n' 'Preserved the certificate signatures produced by Xcode. Notarization has not been performed.'
fi
/usr/bin/codesign --verify --deep --strict --verbose=2 "$APP"
FINAL_APP="$PROJECT_ROOT/dist/Apple Pickup Monitor.app"
rm -rf "$FINAL_APP"
mv "$APP" "$FINAL_APP"
printf 'Native app: %s\nArchitecture: %s\n' "$FINAL_APP" "$BUILD_ARCH"
