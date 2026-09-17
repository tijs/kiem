#!/usr/bin/env bash
# Focused checks for the space-conscious Rust build policy:
#   1. scripts/rust/cleanup-target.sh --help and --dry-run exit 0;
#   2. dry-run lists incremental caches and cross-target trees, deletes
#      nothing, and leaves target/ byte-identical;
#   3. --apply refuses while a cargo-named process is running (guard 1);
#   4. Cargo.toml carries the expected dev-profile policy
#      (incremental = false, deps at line-tables-only, workspace crates full).
# Run from anywhere; requires the repo at <this script>/../..
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT_DIR/scripts/rust/cleanup-target.sh"
CARGO_TOML="$ROOT_DIR/Cargo.toml"
failures=0

check() { # check <description> <result>
  if [[ "$2" == 0 ]]; then
    echo "ok   - $1"
  else
    echo "FAIL - $1"
    failures=$((failures + 1))
  fi
}

# --- 1. help + unknown-arg handling -------------------------------------
"$SCRIPT" --help >/dev/null 2>&1; check "cleanup-target.sh --help exits 0" $?
if "$SCRIPT" --bogus >/dev/null 2>&1; then bogus_rc=0; else bogus_rc=$?; fi
[[ $bogus_rc -ne 0 ]]; check "cleanup-target.sh --bogus exits non-zero" $?

# --- 2. dry run: reports the live state honestly, deletes nothing ---------
out="$("$SCRIPT" --dry-run)"
check "dry-run exits 0" $?

# Mirror the script's own candidate sources so the assertions match the live
# state: before any cleanup there are incremental caches + cross trees + tmp;
# after a cleanup there are none (the zero-candidate no-op path).
has_candidates=false
if find "$ROOT_DIR/target" -type d -name incremental -print 2>/dev/null | grep -q . \
  || [[ -d "$ROOT_DIR/target/aarch64-apple-ios" ]] \
  || [[ -d "$ROOT_DIR/target/tmp" ]]; then
  has_candidates=true
fi

ok=0
[[ "$out" == *"Dry run"* ]] || ok=1
for needle in "target/debug/" "target/release/"; do
  [[ "$out" == *"$needle"* ]] || ok=1
done
if [[ "$has_candidates" == true ]]; then
  for needle in "incremental" "aarch64-apple-ios"; do
    [[ "$out" == *"$needle"* ]] || ok=1
  done
else
  # Explicit empty-candidate dry-run (post-cleanup no-op): must still list the
  # zero set, stay dry, and report a 0-byte total — never an unbound-variable
  # crash (macOS Bash 3.2 + set -u on an empty array).
  [[ "$out" == *"0 disposable candidates"* ]] || ok=1
  [[ "$out" == *"(none)"* ]] || ok=1
  [[ "$out" == *"Total removable: 0 KiB"* ]] || ok=1
  [[ "$out" == *"0 bytes deleted"* ]] || ok=1
fi
check "dry-run output matches live target/ state (candidates present: $has_candidates)" "$ok"

if [[ -d "$ROOT_DIR/target" ]]; then
  before="$(du -sk "$ROOT_DIR/target" | awk '{print $1}')"
  "$SCRIPT" >/dev/null 2>&1
  after="$(du -sk "$ROOT_DIR/target" | awk '{print $1}')"
  [[ "$before" == "$after" ]]; check "dry run deleted nothing (target size unchanged: $before KiB)" $?
fi

# --- 3. --apply refuses while a cargo-named process runs (guard 1) -------
# Spawn a throwaway process literally named "cargo" (argv[0] trick) so
# pgrep -x cargo sees it. Real deletion is NOT exercised: the dry-run +
# guard checks are the contract; existing target trees must never be
# deleted by tests.
if [[ "$(uname -s)" == "Darwin" || "$(uname -s)" == "Linux" ]]; then
  sh -c 'exec -a cargo sleep 8' &
  fake_pid=$!
  sleep 1
  if "$SCRIPT" --apply >/dev/null 2>&1; then rc=0; else rc=$?; fi
  kill "$fake_pid" 2>/dev/null || true
  wait "$fake_pid" 2>/dev/null || true
  [[ $rc -ne 0 ]]; check "--apply refused while a cargo process is running (exit $rc)" $?
fi

# --- 4. Cargo.toml carries the dev-profile policy ------------------------
ok=0
grep -q '^\[profile\.dev\]$' "$CARGO_TOML" || ok=1
grep -q 'incremental = false' "$CARGO_TOML" || ok=1
grep -q 'debug = "line-tables-only"' "$CARGO_TOML" || ok=1
for crate in kiem-core kiem-cli kiem-ffi kiem-sync; do
  grep -q "^\[profile.dev.package.$crate\]$" "$CARGO_TOML" || ok=1
done
check "Cargo.toml dev profile: incremental off + line-tables deps + full workspace debuginfo" "$ok"

echo
if [[ $failures -eq 0 ]]; then
  echo "All policy checks passed."
else
  echo "$failures check(s) FAILED."
  exit 1
fi