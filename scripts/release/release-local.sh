#!/usr/bin/env bash
# Cut a release from this machine. **This is the normal path** — the GitHub
# workflow runs on macos-26 at a 10x minute multiplier and this account's
# Actions credit refreshes monthly, so it is dispatch-only fallback.
#
# Does what .github/workflows/release.yml does, minus the
# throwaway-keychain/cert-import dance — your login keychain already holds the
# Developer ID cert, and notarization uses a stored notarytool profile instead
# of the API key from GitHub secrets.
#
# One-time notarization setup (uses the same App Store Connect .p8 key that
# scripts/release/setup-github-secrets.sh pushed to GitHub):
#   xcrun notarytool store-credentials kiem-notary \
#     --key /path/to/AuthKey_XXXXXXXXXX.p8 \
#     --key-id XXXXXXXXXX \
#     --issuer xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
#
# Usage:
#   scripts/release/release-local.sh 0.1.0-alpha.13
#   scripts/release/release-local.sh v0.1.0-alpha.13   # leading v is fine
#
# Env knobs:
#   NOTARY_KEYCHAIN_PROFILE  notarytool profile name (default: kiem-notary)
#   SKIP_NOTARIZATION=1      build + package + publish without notarizing
#                            (Gatekeeper will flag the app on other Macs)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

VERSION="${1:?usage: release-local.sh <version> (e.g. 0.1.0-alpha.13)}"
VERSION="${VERSION#v}"
TAG="v$VERSION"

# Reproducibility: releases build Pulp at the commit pinned in pulp.ref. Refuse
# to ship from a divergent working copy rather than silently bake in local Pulp
# edits — the runner always builds the pinned ref.
PINNED_PULP="$(cat "$ROOT_DIR/pulp.ref")"
ACTUAL_PULP="$(git -C "$ROOT_DIR/../pulp" rev-parse HEAD 2>/dev/null || echo none)"
if [[ "$ACTUAL_PULP" != "$PINNED_PULP" ]]; then
  echo "error: ../pulp is at $ACTUAL_PULP but pulp.ref pins $PINNED_PULP." >&2
  echo "       Check out the pinned ref (git -C ../pulp checkout $PINNED_PULP)" >&2
  echo "       or bump pulp.ref if this release should pick up new Pulp work." >&2
  exit 1
fi

# Same signing wiring the workflow passes to the scripts, for a Developer ID
# identity already in the login keychain.
export APPLE_TEAM_ID="${APPLE_TEAM_ID:-9Z77B473HX}"
export CODE_SIGN_STYLE="${CODE_SIGN_STYLE:-Manual}"
export CODE_SIGN_IDENTITY="${CODE_SIGN_IDENTITY:-Developer ID Application}"
export SIGNING_IDENTITY="${SIGNING_IDENTITY:-Developer ID Application}"
export NOTARY_KEYCHAIN_PROFILE="${NOTARY_KEYCHAIN_PROFILE:-kiem-notary}"
export VERSION

DMG="$("$ROOT_DIR/scripts/release/release.sh" | tail -n 1)"
echo "Built: $DMG"

# Publish (or update) the GitHub release with the DMG + its checksum.
if gh release view "$TAG" >/dev/null 2>&1; then
  gh release upload "$TAG" "$DMG" "$DMG.sha256" --clobber
else
  gh release create "$TAG" "$DMG" "$DMG.sha256" \
    --title "Kiem $VERSION" \
    --prerelease \
    --notes-file "$ROOT_DIR/CHANGELOG.md"
fi
echo "Published $TAG"
