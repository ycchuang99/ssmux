#!/bin/sh
set -eu

binary=${1:?usage: notarize-macos.sh PATH_TO_SSMUX_BINARY OUTPUT_ZIP}
output=${2:?usage: notarize-macos.sh PATH_TO_SSMUX_BINARY OUTPUT_ZIP}
: "${MACOS_CERTIFICATE_BASE64:?Configure MACOS_CERTIFICATE_BASE64}"
: "${MACOS_CERTIFICATE_PASSWORD:?Configure MACOS_CERTIFICATE_PASSWORD}"
: "${MACOS_SIGNING_IDENTITY:?Configure MACOS_SIGNING_IDENTITY}"
: "${APPLE_ID:?Configure APPLE_ID}"
: "${APPLE_TEAM_ID:?Configure APPLE_TEAM_ID}"
: "${APPLE_APP_SPECIFIC_PASSWORD:?Configure APPLE_APP_SPECIFIC_PASSWORD}"
case "$MACOS_SIGNING_IDENTITY" in
  "Developer ID Application: "*) ;;
  *) echo "MACOS_SIGNING_IDENTITY must be a Developer ID Application identity" >&2; exit 1 ;;
esac

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bundle="$project_dir/target/SSMux.app"
umask 077
work=$(mktemp -d "${TMPDIR:-/tmp}/ssmux-signing.XXXXXX")
keychain="$work/signing.keychain-db"
cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

printf '%s' "$MACOS_CERTIFICATE_BASE64" | base64 --decode > "$work/certificate.p12"
keychain_password=$(openssl rand -hex 32)
security create-keychain -p "$keychain_password" "$keychain" >/dev/null
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$work/certificate.p12" -k "$keychain" -P "$MACOS_CERTIFICATE_PASSWORD" \
  -t cert -f pkcs12 -T /usr/bin/codesign -T /usr/bin/security >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
  -k "$keychain_password" "$keychain" >/dev/null
xcrun notarytool store-credentials ssmux-release --keychain "$keychain" \
  --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_APP_SPECIFIC_PASSWORD" --no-validate >/dev/null

codesign --force --options runtime --timestamp --keychain "$keychain" \
  --sign "$MACOS_SIGNING_IDENTITY" "$binary"
codesign --verify --strict "$binary"
SSMUX_SIGNING_IDENTITY="$MACOS_SIGNING_IDENTITY" SSMUX_SIGNING_KEYCHAIN="$keychain" \
  "$project_dir/scripts/package-macos-menubar.sh" "$binary"

mkdir "$work/submission"
cp "$binary" "$work/submission/ssmux"
ditto "$bundle" "$work/submission/SSMux.app"
ditto -c -k --sequesterRsrc --keepParent "$work/submission" "$work/submission.zip"
xcrun notarytool submit "$work/submission.zip" --keychain-profile ssmux-release \
  --keychain "$keychain" --wait --timeout 20m --output-format json > "$work/result.json"
python3 - "$work/result.json" <<'PY'
import json
import sys

with open(sys.argv[1]) as source:
    result = json.load(source)
print(f"Notarization status: {result.get('status', 'unknown')} ({result.get('id', 'unknown')})")
if result.get("status") != "Accepted":
    sys.exit("Apple did not accept the submission; no release archive was produced.")
PY
xcrun stapler staple "$bundle"
xcrun stapler validate "$bundle"
codesign --verify --strict --verbose=2 "$bundle"
spctl --assess --type execute --verbose=2 "$bundle"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$work/release.zip"
mv "$work/release.zip" "$output"
