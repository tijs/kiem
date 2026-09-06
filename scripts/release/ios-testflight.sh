#!/usr/bin/env bash
# Build + upload the Kiem iOS app to TestFlight via the asc CLI.
#
# Prerequisites (verified on this Mac 2026-09-05):
#   - Apple Distribution cert in the login keychain (team 9Z77B473HX)
#   - "Kiem iOS App Store" provisioning profile installed locally
#     (asc profiles local install --path <download>.mobileprovision)
#   - An App Store Connect app record for org.tijs.kiem.ios (portal: Apps -> +)
#   - asc CLI authenticated (profile: account-holder for upload)
#
# Usage:
#   scripts/release/ios-testflight.sh <ASC_APP_ID>            # full: build -> upload
#   SKIP_UPLOAD=1 scripts/release/ios-testflight.sh <ASC_APP_ID>   # archive+export only
#
# Env knobs:
#   ASC_PROFILE   asc auth profile for the upload (default: account-holder)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APPLE_DIR="$ROOT_DIR/apple"
BUILD_DIR="${BUILD_DIR:-$ROOT_DIR/build/ios}"
ARCHIVE_PATH="$BUILD_DIR/Kiem-iOS.xcarchive"
EXPORT_PATH="$BUILD_DIR/export"
IPA_PATH="$EXPORT_PATH/Kiem iOS.ipa"
SCHEME="Kiem iOS"
PROFILE_NAME="Kiem iOS App Store"
TEAM_ID="9Z77B473HX"
ASC_APP_ID="${1:?usage: ios-testflight.sh <App Store Connect app ID>}"
ASC_PROFILE="${ASC_PROFILE:-account-holder}"

# The embedded Rust core (KiemKit) is a generated XCFramework; rebuild it if any
# Rust source is newer than the last generation (mirrors the macOS archive.sh guard).
ref="$APPLE_DIR/KiemKit/Sources/KiemKit/kiem_ffi.swift"
if [ ! -f "$ref" ] || [ -n "$(find "$ROOT_DIR/crates" -path '*/src/*' -name '*.rs' -newer "$ref" -print -quit)" ]; then
  echo ">> KiemKit is stale — regenerating (release)…"
  "$APPLE_DIR/build-kiemkit.sh" --release
else
  echo ">> KiemKit is fresh — skipping regeneration"
fi

echo ">> Regenerating Xcode project…"
( cd "$APPLE_DIR" && xcodegen generate ) >&2

rm -rf "$ARCHIVE_PATH" "$EXPORT_PATH"
mkdir -p "$BUILD_DIR"

echo ">> Archiving (Manual signing, $PROFILE_NAME)…"
xcodebuild archive \
  -project "$APPLE_DIR/Kiem.xcodeproj" \
  -scheme "$SCHEME" \
  -configuration Release \
  -destination 'generic/platform=iOS' \
  -archivePath "$ARCHIVE_PATH" \
  -derivedDataPath "$BUILD_DIR/DerivedData" \
  -disableAutomaticPackageResolution \
  CODE_SIGN_STYLE=Manual \
  DEVELOPMENT_TEAM="$TEAM_ID" \
  CODE_SIGN_IDENTITY="Apple Distribution" \
  PROVISIONING_PROFILE_SPECIFIER="$PROFILE_NAME" \
  | xcsift

echo ">> Exporting app-store IPA…"
cat > "$BUILD_DIR/ExportOptions.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>method</key>
	<string>app-store</string>
	<key>teamID</key>
	<string>$TEAM_ID</string>
	<key>signingStyle</key>
	<string>manual</string>
	<key>stripSwiftSymbols</key>
	<true/>
	<key>provisioningProfiles</key>
	<dict>
		<key>org.tijs.kiem.ios</key>
		<string>$PROFILE_NAME</string>
	</dict>
</dict>
</plist>
EOF
xcodebuild -exportArchive \
  -archivePath "$ARCHIVE_PATH" \
  -exportPath "$EXPORT_PATH" \
  -exportOptionsPlist "$BUILD_DIR/ExportOptions.plist" \
  | xcsift

[ -f "$IPA_PATH" ] || { echo "error: no IPA at $IPA_PATH" >&2; exit 1; }
echo ">> IPA ready: $IPA_PATH"
codesign -dvv "$EXPORT_PATH/Kiem iOS.app" 2>&1 | grep -E "Identifier|Authority" || true

if [ "${SKIP_UPLOAD:-0}" = "1" ]; then
  echo ">> SKIP_UPLOAD=1 — not uploading. Next:"
  echo "   scripts/release/ios-testflight.sh $ASC_APP_ID"
  exit 0
fi

echo ">> Uploading to App Store Connect (profile: $ASC_PROFILE)…"
asc --profile "$ASC_PROFILE" builds upload --app "$ASC_APP_ID" --ipa "$IPA_PATH"

echo ">> Waiting for processing…"
asc --profile "$ASC_PROFILE" builds wait --app "$ASC_APP_ID" --latest

echo ">> Declaring export compliance (standard encryption only)…"
asc --profile "$ASC_PROFILE" builds update --app "$ASC_APP_ID" --latest --uses-non-exempt-encryption=false || \
  echo ">> (export compliance update skipped — may already be set)"

echo ">> TestFlight status:"
asc --profile "$ASC_PROFILE" testflight pre-release list --app "$ASC_APP_ID" --output table