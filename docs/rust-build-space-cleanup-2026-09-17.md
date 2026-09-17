# Rust build-space cleanup — 2026-09-17

## Scope

This records the authorized cleanup of disposable Rust/Cargo intermediates in
`/Users/tijs/projects/kiem/target` after adding the compact build policy.

The pre-cleanup discovery report measured approximately 48.320 GiB free on `/`.
No Cargo, rustc, benchmark runner, or model server process was active at the
cleanup preflight. `apple/KiemKit`, `target/debug/kiem`, and
`target/release/kiem` were present before cleanup.

## Changes used

- `af4777b` — add compact Cargo profiles and the guarded cleanup command.
- `fbf03e7` — make cleanup safe when Bash 3.2 expands empty arrays, and add
  regression coverage for both candidate-present and post-cleanup no-op states.

## Cleanup

Command run from the Kiem repository:

```text
scripts/rust/cleanup-target.sh --apply
```

The command removed only the disposable candidates reported under `target/`:
Cargo incremental directories and cross-target build trees. It retained host
`target/debug/` and `target/release/` outputs (except their incremental
subdirectories), and did not touch `dist/`, `build/`, or `apple/KiemKit`.

The cleanup command reported **30.8 GiB** removable and removed those candidate
paths. The resulting `target/` size is `19,614,912 KiB` (~18.7 GiB).

## Verification

- `scripts/rust/test-cleanup-target.sh` passed in the real post-cleanup state.
- The cleanup dry run reports `0 disposable candidates` and `0 bytes deleted`.
- The temporary fixture test covered candidate-present cleanup, retained files,
  repeated empty cleanup, the artifact-presence guard, and symlink confinement.
- `cargo test --workspace --offline` passed before cleanup.
- `cargo metadata --no-deps --offline --format-version 1` passed after cleanup.
- `/Users/tijs/bin/kiem --version` returned `kiem 0.4.2` after cleanup.
- Post-cleanup `shutil.disk_usage('/')` reported `75,117,449,216` free bytes
  (`69.959 GiB`).
- The Kiem worktree is clean; no public push was made.

No benchmark or model-server process was started by this cleanup.
