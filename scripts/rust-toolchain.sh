#!/bin/bash
# Shared by Cargo, target setup, and Xcode's helper build. Source this file.

pickup_resolve_rust_toolchain() {
  local project_root="$1" cargo_dir compiler
  if [[ -n "${APPLE_PICKUP_CARGO:-}" ]]; then
    PICKUP_CARGO="$APPLE_PICKUP_CARGO"
  elif command -v cargo >/dev/null 2>&1; then
    PICKUP_CARGO="$(command -v cargo)"
  elif [[ -x "$project_root/.tools/cargo/bin/cargo" ]]; then
    PICKUP_CARGO="$project_root/.tools/cargo/bin/cargo"
    export CARGO_HOME="${CARGO_HOME:-$project_root/.tools/cargo}"
    export RUSTUP_HOME="${RUSTUP_HOME:-$project_root/.tools/rustup}"
  else
    printf '%s\n' 'Rust Cargo was not found. Install Rust, use the project .tools/ toolchain, or set APPLE_PICKUP_CARGO.' >&2
    return 1
  fi

  if [[ "$PICKUP_CARGO" != */* ]]; then
    PICKUP_CARGO="$(command -v "$PICKUP_CARGO")" || return 1
  fi
  if [[ ! -x "$PICKUP_CARGO" ]]; then
    printf 'Cargo is not executable: %s\n' "$PICKUP_CARGO" >&2
    return 1
  fi
  cargo_dir="$(cd "$(dirname "$PICKUP_CARGO")" && pwd)"
  PICKUP_CARGO="$cargo_dir/$(basename "$PICKUP_CARGO")"

  # Explicit compiler selection wins. Otherwise pair Cargo with the compiler
  # installed beside it; this also handles project-local rustup proxies.
  compiler="${RUSTC:-${CARGO_BUILD_RUSTC:-}}"
  if [[ -z "$compiler" ]]; then
    if [[ -x "$cargo_dir/rustc" ]]; then compiler="$cargo_dir/rustc"
    elif command -v rustc >/dev/null 2>&1; then compiler="$(command -v rustc)"
    else
      printf '%s\n' 'Rust compiler was not found. Install the selected toolchain or set RUSTC.' >&2
      return 1
    fi
  fi
  if [[ "$compiler" != */* ]]; then compiler="$(command -v "$compiler")" || return 1; fi
  if [[ ! -x "$compiler" ]]; then
    printf 'Rust compiler is not executable: %s\n' "$compiler" >&2
    return 1
  fi
  export RUSTC="$(cd "$(dirname "$compiler")" && pwd)/$(basename "$compiler")"
  PICKUP_RUSTUP=""
  if [[ -x "$cargo_dir/rustup" ]]; then PICKUP_RUSTUP="$cargo_dir/rustup"
  elif command -v rustup >/dev/null 2>&1; then PICKUP_RUSTUP="$(command -v rustup)"
  fi
}

pickup_print_target_setup() {
  local project_root="$1" variable
  shift
  printf '  env'
  for variable in CARGO_HOME RUSTUP_HOME RUSTUP_TOOLCHAIN; do
    if [[ -n "${!variable:-}" ]]; then printf ' %q' "$variable=${!variable}"; fi
  done
  printf ' %q %q %q' "APPLE_PICKUP_CARGO=$PICKUP_CARGO" "RUSTC=$RUSTC" "$project_root/scripts/setup-rust-targets.sh"
  printf ' %q' "$@"
  printf '\n'
}

pickup_require_rust_targets() {
  local project_root="$1" target library_dir
  shift
  local missing=()
  for target in "$@"; do
    library_dir="$("$RUSTC" --print target-libdir --target "$target")" || return 1
    if ! compgen -G "$library_dir/libcore-*.rlib" >/dev/null || ! compgen -G "$library_dir/libstd-*.rlib" >/dev/null; then
      missing+=("$target")
    fi
  done
  if [[ ${#missing[@]} -gt 0 ]]; then
    printf 'error: The selected Rust toolchain is missing the standard library for: %s\n' "${missing[*]}" >&2
    printf 'Compiler: %s\nInstall the missing targets once, then build again:\n' "$RUSTC" >&2
    pickup_print_target_setup "$project_root" "${missing[@]}" >&2
    return 1
  fi
}
