#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/pickup-engine-lifecycle.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT
xcrun --sdk macosx swiftc -parse-as-library -swift-version 5 \
  -module-cache-path "$PROJECT_ROOT/target/SwiftModuleCache" \
  "$PROJECT_ROOT/native/Localization.swift" "$PROJECT_ROOT/native/Models.swift" \
  "$PROJECT_ROOT/native/EngineTransport.swift" "$PROJECT_ROOT/native/MonitorModel.swift" \
  "$PROJECT_ROOT/native/Tests/EngineLifecycleTests.swift" \
  -o "$TEST_ROOT/lifecycle-tests"
"$TEST_ROOT/lifecycle-tests"
