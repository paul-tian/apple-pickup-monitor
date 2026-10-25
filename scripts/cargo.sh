#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

source "$PROJECT_ROOT/scripts/rust-toolchain.sh"
pickup_resolve_rust_toolchain "$PROJECT_ROOT"
exec "$PICKUP_CARGO" "$@"
