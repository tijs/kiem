#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APPLE_DIR="$ROOT_DIR/apple"
BUILD_DIR="${BUILD_DIR:-"$ROOT_DIR/build/release"}"
ARCHIVE_PATH="${ARCHIVE_PATH:-"$BUILD_DIR/Kiem.xcarchive"}"
EXPORT_PATH="${EXPORT_PATH:-"$BUILD_DIR/export"}"
PROJECT="${PROJECT:-"$APPLE_DIR/Kiem.xcodeproj"}"
SCHEME="${SCHEME:-Kiem}"
CONFIGURATION="${CONFIGURATION:-Release}"
TEAM_ID="${APPLE_TEAM_ID:-}"
CODE_SIGN_STYLE="${CODE_SIGN_STYLE:-Automatic}"
CODE_SIGN_IDENTITY="${CODE_SIGN_IDENTITY:-}"
SIGNING_IDENTITY="${SIGNING_IDENTITY:-Developer ID Application}"
SIGNING_TIMESTAMP="${SIGNING_TIMESTAMP:---timestamp}"

# The embedded Rust core (KiemKit) isn't picked up by Xcode's own dependency
# analysis — a stale one silently ships old behavior instead of failing loudly
# the way the app's own preBuildScript catches it for local dev builds. Always
# regenerate it (release profile) before archiving.
"$APPLE_DIR/build-kiemkit.sh" --release >&2
( cd "$APPLE_DIR" && xcodegen generate ) >&2

rm -rf "$ARCHIVE_PATH" "$EXPORT_PATH"
mkdir -p "$BUILD_DIR"
EXPORT_OPTIONS_PLIST="$BUILD_DIR/ExportOptions.plist"

archive_args=(
  archive
  -project "$PROJECT" \
  -scheme "$SCHEME" \
  -configuration "$CONFIGURATION" \
  -destination "generic/platform=macOS" \
  -archivePath "$ARCHIVE_PATH" \
  CODE_SIGN_STYLE="$CODE_SIGN_STYLE"
)

if [[ -n "$TEAM_ID" ]]; then
  archive_args+=(DEVELOPMENT_TEAM="$TEAM_ID")
fi

if [[ -n "$CODE_SIGN_IDENTITY" ]]; then
  archive_args+=(CODE_SIGN_IDENTITY="$CODE_SIGN_IDENTITY")
fi

# Send build output to stderr so it streams to the CI log; stdout is reserved
# for the final app path that the caller captures with `tail -n 1`.
xcodebuild "${archive_args[@]}" >&2

cat > "$EXPORT_OPTIONS_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>destination</key>
	<string>export</string>
	<key>method</key>
	<string>developer-id</string>
	<key>signingStyle</key>
	<string>automatic</string>
	<key>stripSwiftSymbols</key>
	<true/>
EOF

if [[ -n "$TEAM_ID" ]]; then
  cat >> "$EXPORT_OPTIONS_PLIST" <<EOF
	<key>teamID</key>
	<string>$TEAM_ID</string>
EOF
fi

cat >> "$EXPORT_OPTIONS_PLIST" <<EOF
</dict>
</plist>
EOF

xcodebuild -exportArchive \
  -archivePath "$ARCHIVE_PATH" \
  -exportPath "$EXPORT_PATH" \
  -exportOptionsPlist "$EXPORT_OPTIONS_PLIST" >&2

timestamp_args=()
if [[ "$SIGNING_TIMESTAMP" == "none" ]]; then
  timestamp_args+=(--timestamp=none)
else
  timestamp_args+=("$SIGNING_TIMESTAMP")
fi

# Sign the embedded kiem CLI explicitly, inside-out, before the outer app.
# `--deep` on the app *does* re-sign it (right team, right identity) but
# doesn't reliably carry `--options runtime` down to a loose executable
# sitting in Resources (as opposed to a nested .app/.framework) — notarization
# then rejects the whole bundle over just this one binary missing hardened
# runtime. Apple's own guidance is to sign nested executables explicitly
# rather than lean on --deep for exactly this reason.
codesign --force \
  --options runtime \
  "${timestamp_args[@]}" \
  --sign "$SIGNING_IDENTITY" \
  "$EXPORT_PATH/Kiem.app/Contents/Resources/kiem"

codesign --force --deep \
  --options runtime \
  "${timestamp_args[@]}" \
  --entitlements "$APPLE_DIR/Kiem/Kiem.entitlements" \
  --sign "$SIGNING_IDENTITY" \
  "$EXPORT_PATH/Kiem.app"

echo "$EXPORT_PATH/Kiem.app"
