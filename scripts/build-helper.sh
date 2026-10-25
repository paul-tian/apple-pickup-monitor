#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"
: "${TARGET_BUILD_DIR:?Run this script from the Xcode build phase}"
: "${CONTENTS_FOLDER_PATH:?Xcode did not supply the app bundle location}"

# The shared resolver finds repo-local tools even in Xcode's minimal PATH.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
BUILD_ROOT="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
if [[ "$BUILD_ROOT" != /* ]]; then BUILD_ROOT="$PROJECT_ROOT/$BUILD_ROOT"; fi

BUILD_ARCHS="${ARCHS:-$(uname -m)}"
if [[ "${ONLY_ACTIVE_ARCH:-NO}" == YES && "$BUILD_ARCHS" == *' '* ]]; then
  BUILD_ARCHS="${NATIVE_ARCH_ACTUAL:-$(uname -m)}"
fi
source "$PROJECT_ROOT/scripts/rust-toolchain.sh"
pickup_resolve_rust_toolchain "$PROJECT_ROOT"
RUST_TARGETS=()
for architecture in $BUILD_ARCHS; do
  case "$architecture" in
    arm64) RUST_TARGETS+=(aarch64-apple-darwin) ;;
    x86_64) RUST_TARGETS+=(x86_64-apple-darwin) ;;
    *) printf 'Unsupported helper architecture: %s\n' "$architecture" >&2; exit 1 ;;
  esac
done
# Check the whole architecture set before compiling even the first helper.
pickup_require_rust_targets "$PROJECT_ROOT" "${RUST_TARGETS[@]}"
pickup_resolve_cmake "$PROJECT_ROOT"
BINARIES=()
for rust_target in "${RUST_TARGETS[@]}"; do
  "$PICKUP_CARGO" build --release --locked --bin apple-pickup-monitor --target "$rust_target"
  binary="$BUILD_ROOT/$rust_target/release/apple-pickup-monitor"
  while IFS= read -r library; do
    case "$library" in
      /System/Library/*|/usr/lib/*) ;;
      *) printf 'Unbundled helper runtime dependency: %s\n' "$library" >&2; exit 1 ;;
    esac
  done < <(/usr/bin/otool -L "$binary" | /usr/bin/awk 'NR > 1 { print $1 }')
  BINARIES+=("$binary")
done

HELPERS="$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH/Helpers"
mkdir -p "$HELPERS"
if [[ "${#BINARIES[@]}" == 1 ]]; then
  /usr/bin/install -m 755 "${BINARIES[0]}" "$HELPERS/apple-pickup-monitor"
else
  /usr/bin/lipo -create "${BINARIES[@]}" -output "$HELPERS/apple-pickup-monitor"
  /bin/chmod 755 "$HELPERS/apple-pickup-monitor"
fi

identity="${EXPANDED_CODE_SIGN_IDENTITY:--}"
# Rust's linker signature can use a build-specific UUID as its identifier.
# Keep the Keychain client's identity stable across certificate-signed updates.
HELPER_IDENTIFIER="com.paultian.apple-pickup-monitor.engine"
if [[ -z "$identity" || "$identity" == - ]]; then
  /usr/bin/codesign --force --options runtime --identifier "$HELPER_IDENTIFIER" --sign - "$HELPERS/apple-pickup-monitor"
else
  /usr/bin/codesign --force --options runtime --timestamp --identifier "$HELPER_IDENTIFIER" --sign "$identity" "$HELPERS/apple-pickup-monitor"
fi
