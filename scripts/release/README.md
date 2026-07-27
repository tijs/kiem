# Releasing Kiem

A release builds the macOS app, signs it with the Developer ID identity,
notarizes + staples the DMG, and publishes a GitHub prerelease with the DMG and
its SHA-256.

## Normal path — build on this machine

```bash
# 1. Bump: crates/*/Cargo.toml, apple/project.yml MARKETING_VERSION +
#    CURRENT_PROJECT_VERSION, CHANGELOG.md (move Unreleased into a dated
#    section). Rebuild Cargo.lock (cargo check --workspace). Bump pulp.ref if
#    the release should pick up new Pulp work (and push pulp main first).
# 2. Commit, tag, push:
git commit -am "chore: release 0.3.0"
git tag v0.3.0
git push origin main --tags
# 3. Build, notarize and publish:
scripts/release/release-local.sh 0.3.0
```

That runs `archive.sh` → `package-dmg.sh` → `notarize.sh`, then
`gh release create`. It refuses to start if `../pulp` is not at the commit
`pulp.ref` pins, so a release can never silently bake in local Pulp edits.

To smoke-test the build without notarizing (the app will trip Gatekeeper on
other Macs):

```bash
SKIP_NOTARIZATION=1 scripts/release/release-local.sh 0.3.0
```

This machine has to be on the current Xcode/macOS generation — the same one the
`macos-26` runner uses — because dev builds only ever run against the current
SDK.

## Fallback path — GitHub Actions

`.github/workflows/release.yml` does the same work on a `macos-26` runner. It
is **manual dispatch only**, not tag-triggered:

```bash
gh workflow run release.yml -f version=0.3.0
```

The runner bills at a 10x minute multiplier and this account's Actions credit
refreshes monthly, so in practice it is rarely the cheaper option. Every tag
push from `v0.1.0-alpha.15` through `v0.3.0` was rejected before the job even
started ("recent account payments have failed or your spending limit needs to
be increased") — the DMGs on all of those releases were built locally. The tag
trigger was removed because its only remaining effect was a red X on the
release commit. Use this when there is credit to spend, or when releasing from
a machine that cannot sign.

### One-time local setup

- **Developer ID cert** — must be in your login keychain (it already is if you
  build/sign apps on this machine). Check:
  `security find-identity -v -p codesigning | grep "Developer ID Application"`.
- **Notarization profile** — register the App Store Connect API key (`.p8`) as a
  notarytool keychain profile named `kiem-notary` (what `release-local.sh`
  expects by default). This is the same key `setup-github-secrets.sh` pushed to
  GitHub; the issuer ID is stashed in the login keychain:

  ```bash
  ISSUER=$(security find-generic-password -s kiem-appstoreconnect-issuer-id -w)
  xcrun notarytool store-credentials kiem-notary \
    --key /path/to/AuthKey_RZ7HJR5UJG.p8 \
    --key-id RZ7HJR5UJG \
    --issuer "$ISSUER"
  ```

## Scripts

| Script | Role |
|---|---|
| `release-local.sh` | The release. Build + notarize + publish, end to end. |
| `release.sh` | Build + package + notarize (no publish). `release-local.sh` and the CI job both call it. |
| `archive.sh` | Regenerates KiemKit, `xcodegen`, archives, exports, signs the app + embedded CLI. |
| `package-dmg.sh` | Wraps the `.app` in a DMG + checksum. |
| `notarize.sh` | Submits to Apple, staples, re-checksums. Uses `NOTARY_KEYCHAIN_PROFILE`. |
| `setup-github-secrets.sh` | One-time: pushes signing/notary secrets to GitHub for the fallback path. |
