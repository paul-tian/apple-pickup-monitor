#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/apple-pickup-model-tests.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT

python3 "$PROJECT_ROOT/scripts/check-localizations.py"
for resources in "$PROJECT_ROOT"/native/*.lproj; do
  if [[ -d "$resources" ]]; then cp -R "$resources" "$TEST_ROOT/"; fi
done

xcrun --sdk macosx swiftc -swift-version 5 \
  -module-cache-path "$TEST_ROOT/module-cache" \
  "$PROJECT_ROOT/native/Localization.swift" "$PROJECT_ROOT/native/Models.swift" "$PROJECT_ROOT/native/Tests/main.swift" \
  -o "$TEST_ROOT/native-model-tests"
"$TEST_ROOT/native-model-tests"
