#!/usr/bin/env bash
# One-time setup: pushes the GitHub Actions secrets scripts/release/*.sh and
# .github/workflows/release.yml need, onto this repo. Run locally — the
# credential material below never needs to leave your machine except as
# `gh secret set` payloads (which GitHub encrypts at rest and never displays
# back, even to you).
#
# Before running this, do the one step that has to be done by hand in
# Keychain Access (there's no reliable non-interactive way to export a single
# identity's private key without risking exporting the wrong one, or all of
# them):
#   1. Open Keychain Access, "My Certificates".
#   2. Find "Developer ID Application: <you>" (the same one already used for
#      other apps is fine — one certificate signs any number of your apps).
#   3. Right-click -> Export, save as a .p12, set a password when prompted.
#
# You'll also need an App Store Connect API key (.p8) for notarization. If
# you already made one for another app, reuse it — the same key works across
# all your apps under one team. Otherwise create one at
# https://appstoreconnect.apple.com/access/api (needs the "Developer" role).
#
# Usage:
#   scripts/release/setup-github-secrets.sh \
#     --p12 /path/to/DeveloperIDApplication.p12 \
#     --p12-password '...' \
#     --api-key /path/to/AuthKey_XXXXXXXXXX.p8 \
#     --api-key-id XXXXXXXXXX \
#     --api-issuer-id xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx \
#     [--team-id 9Z77B473HX] \
#     [--repo tijs/kiem]

set -euo pipefail

REPO="tijs/kiem"
TEAM_ID="9Z77B473HX"
P12_PATH=""
P12_PASSWORD=""
API_KEY_PATH=""
API_KEY_ID=""
API_ISSUER_ID=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --p12) P12_PATH="$2"; shift 2 ;;
    --p12-password) P12_PASSWORD="$2"; shift 2 ;;
    --api-key) API_KEY_PATH="$2"; shift 2 ;;
    --api-key-id) API_KEY_ID="$2"; shift 2 ;;
    --api-issuer-id) API_ISSUER_ID="$2"; shift 2 ;;
    --team-id) TEAM_ID="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

: "${P12_PATH:?--p12 is required}"
: "${P12_PASSWORD:?--p12-password is required}"
: "${API_KEY_PATH:?--api-key is required}"
: "${API_KEY_ID:?--api-key-id is required}"
: "${API_ISSUER_ID:?--api-issuer-id is required}"

[[ -f "$P12_PATH" ]] || { echo ".p12 not found: $P12_PATH" >&2; exit 66; }
[[ -f "$API_KEY_PATH" ]] || { echo ".p8 API key not found: $API_KEY_PATH" >&2; exit 66; }

# A fresh random password for the temporary CI keychain (not the .p12's own
# password) — CI creates and unlocks a scratch keychain per run. Reads a
# bounded amount up front (not /dev/urandom piped through head -c, which
# SIGPIPEs the writer under `set -o pipefail` when head closes early) and
# truncates in-process instead of via another pipe stage.
_raw_password="$(openssl rand -base64 48 | tr -dc 'A-Za-z0-9')"
KEYCHAIN_PASSWORD="${_raw_password:0:32}"

echo "Setting secrets on $REPO..."
base64 -i "$P12_PATH" | gh secret set BUILD_CERTIFICATE_BASE64 --repo "$REPO"
printf '%s' "$P12_PASSWORD" | gh secret set P12_PASSWORD --repo "$REPO"
printf '%s' "$KEYCHAIN_PASSWORD" | gh secret set KEYCHAIN_PASSWORD --repo "$REPO"
base64 -i "$API_KEY_PATH" | gh secret set APPLE_API_PRIVATE_KEY_BASE64 --repo "$REPO"
printf '%s' "$API_KEY_ID" | gh secret set APPLE_API_KEY_ID --repo "$REPO"
printf '%s' "$API_ISSUER_ID" | gh secret set APPLE_API_ISSUER_ID --repo "$REPO"
printf '%s' "$TEAM_ID" | gh secret set APPLE_TEAM_ID --repo "$REPO"

echo "Done. Verify with: gh secret list --repo $REPO"
