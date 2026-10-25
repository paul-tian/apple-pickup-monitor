#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/apple-pickup-transport-tests.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT
mkdir -p "$TEST_ROOT/data"
cp "$PROJECT_ROOT/native/Tests/fake-engine.py" "$TEST_ROOT/fake-engine"
chmod 755 "$TEST_ROOT/fake-engine"
xcrun --sdk macosx swiftc -parse-as-library -swift-version 5 \
  -module-cache-path "$TEST_ROOT/module-cache" \
  "$PROJECT_ROOT/native/EngineTransport.swift" "$PROJECT_ROOT/native/Tests/TransportTests.swift" \
  -o "$TEST_ROOT/transport-tests"
"$TEST_ROOT/transport-tests" "$TEST_ROOT/fake-engine" "$TEST_ROOT/data"
