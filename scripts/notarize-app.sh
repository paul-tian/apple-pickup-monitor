#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -lt 1 || $# -gt 2 ]]; then
  printf 'Usage: %s KEYCHAIN_PROFILE [APP_PATH]\n' "$0" >&2
  printf '%s\n' 'The profile must already exist in notarytool Keychain storage.' >&2
  exit 2
fi

PROFILE="$1"
APP="${2:-$PROJECT_ROOT/dist/Apple Pickup Monitor.app}"
if [[ ! -d "$APP/Contents" ]]; then
  printf 'App bundle not found: %s\n' "$APP" >&2
  exit 1
fi
APP="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
/usr/bin/codesign --verify --strict --verbose=2 "$APP"
SIGNATURE="$(/usr/bin/codesign --display --verbose=4 "$APP" 2>&1)"
if [[ "$SIGNATURE" != *'Authority=Developer ID Application:'* ]]; then
  printf '%s\n' 'Notarization requires a Developer ID Application signed build. Rebuild with SIGNING_IDENTITY.' >&2
  exit 1
fi
if [[ "$SIGNATURE" != *'(runtime)'* ]]; then
  printf '%s\n' 'The app must be signed with the hardened runtime enabled.' >&2
  exit 1
fi

STAGING_ROOT="$(mktemp -d "$(dirname "$APP")/.notarize.XXXXXX")"
trap 'rm -rf "$STAGING_ROOT"' EXIT
UPLOAD_ZIP="$STAGING_ROOT/upload.zip"
RECEIPT="$STAGING_ROOT/receipt.plist"
/usr/bin/ditto -c -k --keepParent "$APP" "$UPLOAD_ZIP"
/usr/bin/xcrun notarytool submit "$UPLOAD_ZIP" --keychain-profile "$PROFILE" --wait --output-format plist > "$RECEIPT"
STATUS="$(/usr/bin/plutil -extract status raw -o - "$RECEIPT")"
if [[ "$STATUS" != Accepted ]]; then
  printf 'Apple notarization status: %s\n' "$STATUS" >&2
  /usr/bin/plutil -p "$RECEIPT" >&2
  exit 1
fi

/usr/bin/xcrun stapler staple "$APP"
/usr/bin/xcrun stapler validate "$APP"
/usr/sbin/spctl --assess --type execute --verbose=2 "$APP"

RELEASE_ZIP="${APP%.app}-notarized.zip"
/usr/bin/ditto -c -k --keepParent "$APP" "$RELEASE_ZIP"
printf 'Notarized and stapled app: %s\nRelease archive: %s\n' "$APP" "$RELEASE_ZIP"
