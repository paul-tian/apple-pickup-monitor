#!/bin/bash
# Install build tools in this checkout only. Requires macOS, Xcode and internet.
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$PROJECT_ROOT/scripts/build-tools.env"
if [[ $# -gt 0 ]]; then
  if [[ $# == 1 && ( "$1" == --help || "$1" == -h ) ]]; then
    printf '%s\n' 'Usage: ./scripts/setup-build-tools.sh' \
      'Installs pinned Rust and CMake under .tools/ for both macOS architectures.' \
      'No Homebrew, sudo, shell-profile edits or system installation is used.'
    exit 0
  fi
  printf '%s\n' 'Usage: ./scripts/setup-build-tools.sh (no arguments)' >&2
  exit 2
fi
if [[ "$(/usr/bin/uname -s)" != Darwin ]]; then
  printf '%s\n' 'This setup script requires macOS and Xcode.' >&2
  exit 1
fi
case "$(/usr/bin/uname -m)" in
  arm64) RUST_HOST=aarch64-apple-darwin; RUSTUP_SHA256="$PICKUP_RUSTUP_ARM64_SHA256" ;;
  x86_64) RUST_HOST=x86_64-apple-darwin; RUSTUP_SHA256="$PICKUP_RUSTUP_X86_64_SHA256" ;;
  *) printf '%s\n' 'Unsupported Mac architecture.' >&2; exit 1 ;;
esac
# Check existing Xcode setup without changing xcode-select or accepting licenses.
if ! /usr/bin/xcodebuild -version >/dev/null 2>&1 ||
   ! /usr/bin/xcodebuild -checkFirstLaunchStatus >/dev/null 2>&1 ||
   ! /usr/bin/xcrun --sdk macosx --find clang >/dev/null 2>&1 ||
   ! /usr/bin/xcrun --sdk macosx --find swiftc >/dev/null 2>&1; then
  printf '%s\n' 'Open Xcode and finish its first-launch setup, then run this script again.' >&2
  exit 1
fi

TOOLS="$PROJECT_ROOT/.tools"
for directory in "$TOOLS" "$TOOLS/cargo" "$TOOLS/cargo/bin" "$TOOLS/rustup" "$TOOLS/cmake" "$TOOLS/downloads"; do
  if [[ -L "$directory" ]]; then
    printf 'Refusing to install through a symlink: %s\n' "$directory" >&2
    exit 1
  fi
done
mkdir -p "$TOOLS/downloads"
if ! mkdir "$TOOLS/.setup-lock" 2>/dev/null; then
  printf '%s\n' 'Another setup may be running. If it was interrupted, remove .tools/.setup-lock and retry.' >&2
  exit 1
fi
WORK=""
cleanup() {
  if [[ -n "$WORK" ]]; then rm -rf "$WORK"; fi
  rmdir "$TOOLS/.setup-lock" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
WORK="$(mktemp -d "$TOOLS/.setup.XXXXXX")"

# These variables affect this script and its children, never the user's shell.
# Ignore inherited homes and mirrors so setup cannot update a global toolchain.
export CARGO_HOME="$TOOLS/cargo"
export RUSTUP_HOME="$TOOLS/rustup"
export RUSTUP_DIST_SERVER=https://static.rust-lang.org
export RUSTUP_UPDATE_ROOT=https://static.rust-lang.org/rustup
export RUSTUP_AUTO_INSTALL=0
export RUSTUP_INIT_SKIP_PATH_CHECK=yes
unset RUSTUP_TOOLCHAIN
export PATH="$CARGO_HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin"

checksum_matches() {
  local file="$1" expected="$2" actual
  [[ -f "$file" && ! -L "$file" ]] || return 1
  actual="$(/usr/bin/shasum -a 256 "$file")"
  [[ "${actual%% *}" == "$expected" ]]
}

download_verified() {
  local url="$1" filename="$2" expected="$3" cached="$TOOLS/downloads/$2"
  if [[ -L "$cached" || -d "$cached" ]]; then
    printf 'Refusing an unsafe download cache path: %s\n' "$cached" >&2
    return 1
  fi
  if ! checksum_matches "$cached" "$expected"; then
    printf 'Downloading %s…\n' "$filename" >&2
    /usr/bin/curl --fail --location --show-error --silent \
      --proto '=https' --proto-redir '=https' --tlsv1.2 \
      --retry 3 --connect-timeout 20 --max-time 900 \
      --output "$WORK/$filename" "$url"
    if ! checksum_matches "$WORK/$filename" "$expected"; then
      printf 'Checksum verification failed for %s. Nothing from this download was installed.\n' "$filename" >&2
      return 1
    fi
    mv -f "$WORK/$filename" "$cached"
  fi
  printf '%s\n' "$cached"
}

RUSTUP="$CARGO_HOME/bin/rustup"
installed_rustup=""
if [[ -x "$RUSTUP" ]]; then
  installed_rustup="$("$RUSTUP" --version 2>/dev/null | /usr/bin/awk 'NR == 1 {print $2}')" || installed_rustup=""
fi
if [[ "$installed_rustup" != "$PICKUP_RUSTUP_VERSION" ]]; then
  installer="$(download_verified \
    "https://static.rust-lang.org/rustup/archive/$PICKUP_RUSTUP_VERSION/$RUST_HOST/rustup-init" \
    "rustup-init-$PICKUP_RUSTUP_VERSION-$RUST_HOST" "$RUSTUP_SHA256")"
  cp "$installer" "$WORK/rustup-init"
  chmod 755 "$WORK/rustup-init"
  "$WORK/rustup-init" -y --no-modify-path --profile minimal --default-host "$RUST_HOST" --default-toolchain none
fi
"$RUSTUP" set auto-self-update disable
RUST_TOOLCHAIN="$PICKUP_RUST_VERSION-$RUST_HOST"
if ! "$RUSTUP" run "$RUST_TOOLCHAIN" rustc --version >/dev/null 2>&1; then
  printf 'Installing Rust %s and macOS target libraries…\n' "$PICKUP_RUST_VERSION"
  "$RUSTUP" toolchain install "$RUST_TOOLCHAIN" --profile minimal --no-self-update \
    --component rustfmt --component clippy \
    --target aarch64-apple-darwin --target x86_64-apple-darwin
fi
# Repair an interrupted setup without reinstalling already complete components.
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  library_dir="$("$RUSTUP" run "$RUST_TOOLCHAIN" rustc --print target-libdir --target "$target")"
  if ! compgen -G "$library_dir/libcore-*.rlib" >/dev/null || ! compgen -G "$library_dir/libstd-*.rlib" >/dev/null; then
    "$RUSTUP" target add --toolchain "$RUST_TOOLCHAIN" "$target"
  fi
  if ! compgen -G "$library_dir/libcore-*.rlib" >/dev/null || ! compgen -G "$library_dir/libstd-*.rlib" >/dev/null; then
    printf 'Rust target %s is incomplete even after installation. Repair the toolchain in .tools/rustup and rerun setup.\n' "$target" >&2
    exit 1
  fi
done
components="$("$RUSTUP" component list --toolchain "$RUST_TOOLCHAIN" --installed)"
for component in rustfmt clippy; do
  if [[ "$components" != *"$component-$RUST_HOST"* ]]; then
    "$RUSTUP" component add --toolchain "$RUST_TOOLCHAIN" "$component"
  fi
done
"$RUSTUP" default "$RUST_TOOLCHAIN"

CMAKE_BIN="$TOOLS/cmake/CMake.app/Contents/bin/cmake"
installed_cmake=""
if [[ -x "$CMAKE_BIN" ]]; then
  installed_cmake="$("$CMAKE_BIN" --version | /usr/bin/awk 'NR == 1 {print $3}')" || installed_cmake=""
fi
if [[ "$installed_cmake" != "$PICKUP_CMAKE_VERSION" ]]; then
  archive="$(download_verified \
    "https://github.com/Kitware/CMake/releases/download/v$PICKUP_CMAKE_VERSION/cmake-$PICKUP_CMAKE_VERSION-macos-universal.tar.gz" \
    "cmake-$PICKUP_CMAKE_VERSION-macos-universal.tar.gz" "$PICKUP_CMAKE_SHA256")"
  mkdir "$WORK/cmake"
  /usr/bin/tar -xzf "$archive" -C "$WORK/cmake" --strip-components=1
  staged_cmake="$WORK/cmake/CMake.app/Contents/bin/cmake"
  if [[ ! -x "$staged_cmake" ]] ||
     [[ "$("$staged_cmake" --version | /usr/bin/awk 'NR == 1 {print $3}')" != "$PICKUP_CMAKE_VERSION" ]]; then
    printf '%s\n' 'The downloaded CMake installation failed validation.' >&2
    exit 1
  fi
  if [[ -e "$TOOLS/cmake" ]]; then mv "$TOOLS/cmake" "$WORK/previous-cmake"; fi
  if ! mv "$WORK/cmake" "$TOOLS/cmake"; then
    if [[ -d "$WORK/previous-cmake" ]]; then mv "$WORK/previous-cmake" "$TOOLS/cmake"; fi
    exit 1
  fi
fi

"$RUSTUP" run "$RUST_TOOLCHAIN" rustc --version
"$CMAKE_BIN" --version
printf '\nBuild tools are ready in %s\n' "$TOOLS"
printf '%s\n' 'Xcode will find these tools automatically; no shell-profile changes or environment sourcing are needed.'
printf '%s\n' 'Open ApplePickupMonitor.xcodeproj, select your signing team (or Sign to Run Locally), and build.'
