# Rust build space policy

Kiem's Rust builds can consume tens of GiB of disk: `target/` was ~45 GiB at
the time this policy was written, dominated by `target/debug/` (~18 GiB, of
which ~5 GiB was incremental state) and by cargo-swift's multi-platform build
trees — every `apple/build-kiemkit.sh` invocation builds macOS arm64/x86_64,
iOS device, and iOS simulator through cargo, leaving several-GiB Debug and
Release trees under `target/aarch64-apple-*`, `target/x86_64-apple-*`, and
`target/universal-*`.

This policy has two parts: **cap future growth** (workspace profile settings)
and **reclaim disposable outputs on demand** (a guarded cleanup script). It
never deletes a `dist/` DMG, a signed `build/release/export/Kiem.app`, or the
embedded `apple/KiemKit` XCFramework — those are outside `target/` exactly so
they can never be collateral.

## 1. Capping future growth — `[profile.dev]` in the root `Cargo.toml`

The dev profile drives plain `cargo build`/`cargo test`/`cargo clippy` **and**
cargo-swift's non-release KiemKit builds, so it multiplies across every
cross-target tree. Two settings:

- `incremental = false` — no incremental compilation state is written for dev
  builds. Incremental dirs were the single largest consumer (multi-GiB per
  profile per target) and are pure cache. Cost: warm rebuilds recompile whole
  crates; disk stops growing. (Release already defaults to `incremental = off`
  and is untouched.)
- `[profile.dev.package."*"] debug = "line-tables-only"` — dependencies build
  with line tables only: backtraces and panics keep line numbers while object
  sizes roughly halve for iroh/tantivy-sized deps. The four workspace crates
  (`kiem-core`, `kiem-cli`, `kiem-ffi`, `kiem-sync`) are explicitly set back to
  `debug = true` so lldb breakpoints on first-party code are unaffected.

**Unchanged:** the release profile. `scripts/release/*.sh` and
`apple/project.yml` depend on its output paths (`target/release/kiem` embedded
into the app) and behavior; `[profile.dev.package.kiem-*] debug = true` also
applies to dev-profile builds of the CLI the Xcode Debug build embeds.

## 2. Reclaiming disposable outputs — `scripts/rust/cleanup-target.sh`

Run it after a packaged build exists (KiemKit regenerated, a DMG in `dist/`,
or an exported app) to reclaim the build *inputs* that no longer need to stay
warm. **Dry run by default — pass `--apply` to delete anything.**

```sh
scripts/rust/cleanup-target.sh          # list candidates + sizes, delete nothing
scripts/rust/cleanup-target.sh --apply  # delete exactly the same set
```

### Removes (all under `<repo>/target/`)

| Paths | Why disposable |
|---|---|
| every `incremental/` directory, any profile, any target triple | cargo regenerates incremental state on demand; it exists only to speed the next build |
| `target/{aarch64,x86_64}-apple-darwin`, `...-apple-ios`, `...-apple-ios-sim`, `target/universal-ios`, `target/universal-macos` (whole trees, Debug + Release) | cargo-swift intermediates for the KiemKit XCFramework; once `apple/KiemKit` exists the app and every script read that, not these |
| `target/tmp` | cargo scratch |

### Retains (never touched)

| Paths | Why |
|---|---|
| everything in `target/debug/` except its `incremental/` subdir | the daily dev build; Xcode Debug embeds `target/debug/kiem` into the app |
| everything in `target/release/` except its `incremental/` subdir | `scripts/release/archive.sh` / Xcode Release embed `target/release/kiem`; renaming or deleting it would force a full release rebuild or break packaging |
| `dist/`, `build/`, `apple/KiemKit` | packaged/signed outputs — outside `target/` by design |

### Guards (`--apply` only)

1. **No in-flight build.** Refuses if any `cargo` or `rustc` process is
   running — a deletion mid-build can corrupt the build.
2. **Packaged artifact present.** Refuses unless `apple/KiemKit`,
   `dist/Kiem-*.dmg`, or `build/release/export/Kiem.app` exists. Deleting
   build inputs before their output exists just forces a full rebuild.
3. **Path confinement.** `target/` is realpath-resolved and must equal
   `<repo-root>/target` exactly (no symlink escape, no arguments, no globs —
   the script only ever touches that hardcoded path).

Exit codes: `0` ok, `1` build in progress, `2` no packaged artifact, `3` path
check failed, `64` bad usage.

`scripts/rust/test-cleanup-target.sh` exercises this contract without deleting
anything: dry-run lists + byte-identical tree, `--apply` refusal while a
cargo-named process runs, and the Cargo.toml policy keys.

## Notes

- This policy does **not** reclaim the existing ~45 GiB tree — it caps how
  fast it grows and gives a guarded way to reclaim the disposable subset.
  Reclaiming the pre-existing tree is a separate, explicitly-approved cleanup.
- `scripts/release/release-local.sh`, `archive.sh`, `package-dmg.sh`,
  `notarize.sh`, and the GitHub workflow are unchanged: same output paths,
  same failure behavior. Cleanup is deliberately **not** wired into release
  scripts — post-build cleanup is an explicit human step so a release can
  never silently delete a build someone is still using.