# Releasing Kiem

A release builds the macOS app, signs it with the Developer ID identity,
notarizes + staples the DMG, and publishes a GitHub prerelease with the DMG and
its SHA-256.

## Normal path — GitHub Actions (recommended)

Bump versions, then push a tag. The workflow (`.github/workflows/release.yml`,
`macos-26` runner) does the rest.

```bash
# 1. Bump: crates/*/Cargo.toml, apple/project.yml CURRENT_PROJECT_VERSION,
#    CHANGELOG.md. Rebuild Cargo.lock (cargo build). Bump pulp.ref if the
#    release should pick up new Pulp work (and push pulp main first).
# 2. Commit, tag, push:
git tag v0.1.0-alpha.13
git push origin main --tags
```

## Local path — build on this machine

Use when GitHub runner minutes are exhausted. Same scripts, run locally; this
machine must be on the current Xcode/macOS generation (matches the `macos-26`
runner) with `../pulp` checked out at the commit in `pulp.ref`.

```bash
scripts/release/release-local.sh 0.1.0-alpha.13
```

That runs `archive.sh` → `package-dmg.sh` → `notarize.sh`, then
`gh release create`. To smoke-test the build without notarizing (the app will
trip Gatekeeper on other Macs):

```bash
SKIP_NOTARIZATION=1 scripts/release/release-local.sh 0.1.0-alpha.13
```

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
| `release-local.sh` | Local end-to-end: build + notarize + publish (replaces the CI job). |
| `release.sh` | Build + package + notarize (no publish). CI and `release-local.sh` both call it. |
| `archive.sh` | Regenerates KiemKit, `xcodegen`, archives, exports, signs the app + embedded CLI. |
| `package-dmg.sh` | Wraps the `.app` in a DMG + checksum. |
| `notarize.sh` | Submits to Apple, staples, re-checksums. Uses `NOTARY_KEYCHAIN_PROFILE`. |
| `setup-github-secrets.sh` | One-time: pushes signing/notary secrets to GitHub for the CI path. |
