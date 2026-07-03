# Releasing Kiem

Kiem is distributed outside the Mac App Store as a Developer ID signed and
notarized disk image attached to a GitHub release (private repo — releases
are visible only to you and any collaborators you add).

## One-time setup

Already done for this repo (`tijs/kiem`), reusing the same Developer ID
Application certificate and App Store Connect API key as `~/projects/Peel`
— one certificate signs any number of your apps. If it ever needs redoing
(new machine, rotated credentials): export the certificate from Keychain
Access as a `.p12` (no reliable non-interactive way to export a single
identity), then:

```sh
scripts/release/setup-github-secrets.sh \
  --p12 /path/to/cert.p12 --p12-password '...' \
  --api-key /path/to/AuthKey_XXXXXXXXXX.p8 \
  --api-key-id XXXXXXXXXX --api-issuer-id xxxxxxxx-xxxx-...
```

## Local release build

```sh
VERSION=0.1.0-alpha.3 scripts/release/release.sh
```

Set `SKIP_NOTARIZATION=1` for a local packaging dry run that skips the Apple
notary upload. This still requires a Developer ID Application certificate,
because the exported app must use Developer ID signing.

```sh
VERSION=0.1.0-alpha.3 SKIP_NOTARIZATION=1 scripts/release/release.sh
```

The release artifact is written to `dist/Kiem-<version>.dmg` with a matching
`.sha256` checksum. `scripts/release/archive.sh` rebuilds the embedded
KiemKit core (release profile) and regenerates the Xcode project first —
Xcode's own dependency analysis can't see `crates/*.rs` changes.

## GitHub release

1. Add a section to `CHANGELOG.md` for the new version.
2. Bump the version: `crates/*/Cargo.toml` (all four crates, kept in
   lockstep) and `apple/project.yml`'s `MARKETING_VERSION` /
   `CURRENT_PROJECT_VERSION`.
3. `cargo build` once to refresh `Cargo.lock`, then commit.
4. Tag the commit and push:

   ```sh
   git tag v0.1.0-alpha.3
   git push origin main
   git push origin v0.1.0-alpha.3
   ```

The `Release` workflow builds, notarizes, staples, and uploads the `.dmg`
and checksum to the GitHub release, using `CHANGELOG.md` as the release
notes. It also accepts manual triggers (`workflow_dispatch` with a
`version` input) against any ref — useful for re-running a fix without
juggling the tag.

## Verification

After downloading the artifact from a release:

```sh
shasum -a 256 -c Kiem-0.1.0-alpha.3.dmg.sha256
spctl --assess --type open --context context:primary-signature --verbose Kiem-0.1.0-alpha.3.dmg
xcrun stapler validate Kiem-0.1.0-alpha.3.dmg
```

Then mount the disk image and drag `Kiem.app` to `/Applications`. No
right-click-Open workaround needed — it's notarized.
