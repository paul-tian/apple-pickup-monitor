#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"
source "$PROJECT_ROOT/scripts/rust-toolchain.sh"
pickup_resolve_rust_toolchain "$PROJECT_ROOT"

if [[ $# -eq 0 ]]; then set -- aarch64-apple-darwin x86_64-apple-darwin; fi
for target in "$@"; do
  case "$target" in
    aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) printf 'Unsupported macOS Rust target: %s\n' "$target" >&2; exit 1 ;;
  esac
done

# Do not install into rustup's unrelated default toolchain. Resolve the sysroot
# of the exact compiler Cargo will use, then verify rustup manages that compiler.
sysroot="$("$RUSTC" --print sysroot)"
toolchains_dir="$(dirname "$sysroot")"
if [[ -z "$PICKUP_RUSTUP" && -x "$PROJECT_ROOT/.tools/cargo/bin/rustup" ]]; then
  PICKUP_RUSTUP="$PROJECT_ROOT/.tools/cargo/bin/rustup"
fi
if [[ "$(basename "$toolchains_dir")" != toolchains || -z "$PICKUP_RUSTUP" ]]; then
  printf 'The selected compiler is not a discoverable rustup-managed toolchain: %s\n' "$RUSTC" >&2
  printf 'Install the requested target standard libraries through that compiler distribution, or select a rustup toolchain with APPLE_PICKUP_CARGO and RUSTC.\n' >&2
  exit 1
fi
export RUSTUP_HOME="$(dirname "$toolchains_dir")"
toolchain="$(basename "$sysroot")"
managed_sysroot="$("$PICKUP_RUSTUP" run "$toolchain" rustc --print sysroot)"
if [[ "$managed_sysroot" != "$sysroot" ]]; then
  printf '%s\n' 'Refusing to install targets into a toolchain different from the selected compiler.' >&2
  exit 1
fi

printf 'Installing macOS target support into %s\n' "$sysroot"
"$PICKUP_RUSTUP" target add --toolchain "$toolchain" "$@"
pickup_require_rust_targets "$PROJECT_ROOT" "$@"
printf '%s\n' 'Requested Rust targets are ready.'
