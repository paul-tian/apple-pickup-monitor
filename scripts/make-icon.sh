#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"
mkdir -p target/icon-build target/swift-module-cache
/usr/bin/xcrun swiftc -module-cache-path "$PROJECT_ROOT/target/swift-module-cache" scripts/make-icon.swift -o target/icon-build/make-icon
target/icon-build/make-icon "$PROJECT_ROOT/target/icon-build/AppIcon.iconset"
# ICNS is a container of PNG representations. Packing the rendered sizes also
# avoids iconutil conversion differences between macOS build machines.
python3 - <<'PY'
import pathlib
import struct

folder = pathlib.Path('target/icon-build/AppIcon.iconset')
entries = [
    ('icp4', 'icon_16x16.png'), ('icp5', 'icon_32x32.png'),
    ('icp6', 'icon_32x32@2x.png'), ('ic07', 'icon_128x128.png'),
    ('ic08', 'icon_256x256.png'), ('ic09', 'icon_512x512.png'),
    ('ic10', 'icon_512x512@2x.png'), ('ic11', 'icon_16x16@2x.png'),
    ('ic12', 'icon_32x32@2x.png'), ('ic13', 'icon_128x128@2x.png'),
    ('ic14', 'icon_256x256@2x.png'),
]
chunks = []
for kind, name in entries:
    png = (folder / name).read_bytes()
    chunks.append(kind.encode('ascii') + struct.pack('>I', len(png) + 8) + png)
payload = b''.join(chunks)
pathlib.Path('native/AppIcon.icns').write_bytes(b'icns' + struct.pack('>I', len(payload) + 8) + payload)
PY
