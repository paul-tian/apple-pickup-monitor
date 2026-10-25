#!/bin/bash
# Shared by Cargo, target setup, and Xcode's helper build. Source this file.

pickup_resolve_rust_toolchain() {
  local project_root="$1" cargo_dir compiler
  if [[ -n "${APPLE_PICKUP_CARGO:-}" ]]; then
    PICKUP_CARGO="$APPLE_PICKUP_CARGO"
  elif [[ -x "$project_root/.tools/cargo/bin/cargo" ]]; then
    PICKUP_CARGO="$project_root/.tools/cargo/bin/cargo"
  elif command -v cargo >/dev/null 2>&1; then
    PICKUP_CARGO="$(command -v cargo)"
  elif [[ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]]; then
    # Xcode launched from Finder does not inherit a shell's PATH.
    PICKUP_CARGO="${CARGO_HOME:-$HOME/.cargo}/bin/cargo"
  else
    printf 'Rust Cargo was not found. Run %q once, then build again.\n' "$project_root/scripts/setup-build-tools.sh" >&2
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
  if [[ -d "$project_root/.tools/cargo/bin" && "$cargo_dir" == "$(cd "$project_root/.tools/cargo/bin" && pwd)" ]]; then
    # A repo-local rustup proxy must never use unrelated inherited homes.
    export CARGO_HOME="$project_root/.tools/cargo"
    export RUSTUP_HOME="$project_root/.tools/rustup"
    # Missing compilers belong to explicit setup, not an implicit Xcode download.
    export RUSTUP_AUTO_INSTALL=0
  fi
  export PATH="$cargo_dir:$PATH"

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

pickup_resolve_cmake() {
  local project_root="$1" executable version major minor
  if [[ -n "${CMAKE:-}" ]]; then
    executable="$CMAKE"
  elif [[ -x "$project_root/.tools/cmake/CMake.app/Contents/bin/cmake" ]]; then
    executable="$project_root/.tools/cmake/CMake.app/Contents/bin/cmake"
  elif command -v cmake >/dev/null 2>&1; then
    executable="$(command -v cmake)"
  elif [[ -x /Applications/CMake.app/Contents/bin/cmake ]]; then
    executable=/Applications/CMake.app/Contents/bin/cmake
  else
    printf 'CMake was not found. Run %q once, then build again.\n' "$project_root/scripts/setup-build-tools.sh" >&2
    return 1
  fi
  if [[ "$executable" != */* ]]; then
    executable="$(command -v "$executable")" || {
      printf 'CMAKE does not name an available executable. Run %q or set CMAKE to its full path.\n' "$project_root/scripts/setup-build-tools.sh" >&2
      return 1
    }
  fi
  if [[ ! -x "$executable" ]]; then
    printf 'CMake is not executable: %s. Run %q once.\n' "$executable" "$project_root/scripts/setup-build-tools.sh" >&2
    return 1
  fi
  executable="$(cd "$(dirname "$executable")" && pwd)/$(basename "$executable")"
  if ! version="$("$executable" --version)" || [[ ! "$version" =~ ^cmake[[:space:]]version[[:space:]]([0-9]+)\.([0-9]+) ]]; then
    printf 'Could not determine the CMake version at %s. Run %q once.\n' "$executable" "$project_root/scripts/setup-build-tools.sh" >&2
    return 1
  fi
  major="${BASH_REMATCH[1]}"; minor="${BASH_REMATCH[2]}"
  if (( major < 3 || (major == 3 && minor < 22) )); then
    printf 'CMake 3.22 or newer is required; found %s.%s. Run %q once.\n' "$major" "$minor" "$project_root/scripts/setup-build-tools.sh" >&2
    return 1
  fi
  export CMAKE="$executable"
  export PATH="$(dirname "$CMAKE"):$PATH"
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
