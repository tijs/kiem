#!/usr/bin/env bash
# Reclaim disposable Cargo target outputs from the Kiem workspace after a
# packaged build (apple/KiemKit XCFramework, dist/*.dmg, or the exported
# Kiem.app) is safely present. DRY-RUN BY DEFAULT — nothing is deleted unless
# you pass --apply.
#
# What it removes (every path is inside <repo>/target/):
#   - all incremental/ caches, any profile, any target triple  (regenerable)
#   - cargo-swift's cross-target build trees:
#       target/{aarch64,x86_64}-apple-darwin|{aarch64,x86_64}-apple-ios|
#       aarch64-apple-ios-sim|universal-ios|universal-macos
#     (only kept while KiemKit is being built; the durable artifact is
#     apple/KiemKit or the DMG — the app never reads these trees again)
#   - target/tmp scratch
#
# What it RETAINS (deliberately — these are the daily-use builds):
#   - everything under target/debug/ and target/release/ except their
#     incremental/ subdirectories. target/release/kiem is what
#     apple/project.yml embeds into the app in Release builds, and
#     target/debug/ is the dev build used by plain cargo test/dev.
#   - dist/, build/, apple/KiemKit — outside target/, never touched.
#
# Guards (apply mode):
#   1. refuses if a cargo/rustc process is running (could be mid-build);
#   2. refuses if no packaged artifact exists yet (apple/KiemKit, dist/*.dmg,
#      or build/release/export/Kiem.app) — deleting inputs before the output
#      exists would just force a full rebuild for nothing;
#   3. verifies the real path of target/ is exactly <repo-root>/target before
#      touching anything (no symlink escape, no arbitrary paths).
#
# Usage:
#   scripts/rust/cleanup-target.sh            # dry run: list + sizes, delete nothing
#   scripts/rust/cleanup-target.sh --apply    # actually remove the same set
#   scripts/rust/cleanup-target.sh --help
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
TARGET_DIR="$ROOT_DIR/target"

# Everything under target/ that cargo-swift builds for the KiemKit XCFramework
# (macOS arm64/x86_64, iOS device/simulator, lipo'd universals). Once
# apple/KiemKit exists these are pure intermediates.
CROSS_TRIPLES=(
  aarch64-apple-darwin
  aarch64-apple-ios
  aarch64-apple-ios-sim
  x86_64-apple-darwin
  x86_64-apple-ios
  universal-ios
  universal-macos
)

usage() {
  sed -n '2,45p' "$0" | sed 's/^# \?//'
  exit 0
}

APPLY=0
for arg in "$@"; do
  case "$arg" in
    --apply|--yes) APPLY=1 ;;
    --dry-run) APPLY=0 ;;
    -h|--help) usage ;;
    *)
      echo "error: unknown option: $arg (try --help)" >&2
      exit 64
      ;;
  esac
done

if [[ ! -d "$TARGET_DIR" ]]; then
  echo "No $TARGET_DIR — nothing to clean."
  exit 0
fi

# Guard 3: target/ must resolve exactly to <repo-root>/target.
resolved_target="$(cd "$TARGET_DIR" && pwd -P)"
if [[ "$resolved_target" != "$ROOT_DIR/target" ]]; then
  echo "error: $TARGET_DIR resolves to $resolved_target, not $ROOT_DIR/target — refusing." >&2
  exit 3
fi

# Build the candidate list (stable order: incremental caches, then cross trees,
# then scratch).
incremental_dirs=()
while IFS= read -r d; do
  [[ -z "$d" ]] && continue
  incremental_dirs+=("$d")
done < <(find "$TARGET_DIR" -type d -name incremental -print 2>/dev/null | sort)

cross_candidates=()
if (( ${#CROSS_TRIPLES[@]} > 0 )); then
  for t in "${CROSS_TRIPLES[@]}"; do
    if [[ -d "$TARGET_DIR/$t" ]]; then
      cross_candidates+=("$TARGET_DIR/$t")
    fi
  done
fi

# Incremental dirs nested inside a cross-target tree are removed with that
# tree — keep only the ones that are standalone (host target/debug|release)
# so the list is non-overlapping and the freed total is not double-counted.
#
# macOS ships Bash 3.2, where `set -u` treats "${arr[@]}" on an empty array as
# an unbound-variable error (fixed only in Bash 4.4). Every array expansion
# here is guarded by an explicit length check so the script stays a clean no-op
# after a previous cleanup has already removed all candidates.
candidates=()
if (( ${#incremental_dirs[@]} > 0 )); then
  for d in "${incremental_dirs[@]}"; do
    nested=false
    if (( ${#cross_candidates[@]} > 0 )); then
      for t in "${cross_candidates[@]}"; do
        case "$d" in
          "$t"/*) nested=true; break ;;
        esac
      done
    fi
    [[ "$nested" == false ]] && candidates+=("$d")
  done
fi
if (( ${#cross_candidates[@]} > 0 )); then
  candidates+=("${cross_candidates[@]}")
fi
[[ -d "$TARGET_DIR/tmp" ]] && candidates+=("$TARGET_DIR/tmp")

byte_count() { # dir -> whole KiB as a plain integer (du -sk, no suffix)
  du -sk "$1" 2>/dev/null | awk '{print $1}'
}

fmt_kib() { # KiB -> human string
  awk -v k="$1" 'BEGIN{
    if (k >= 1048576) printf "%.1f GiB", k/1048576
    else if (k >= 1024) printf "%.1f MiB", k/1024
    else printf "%d KiB", k
  }'
}

total=0
echo "=== Dry run: $((${#candidates[@]})) disposable candidates under target/ ==="
if [[ "$APPLY" == 1 ]]; then echo "(apply mode)"; else echo "(dry run — nothing will be deleted; pass --apply to delete)"; fi
if (( ${#candidates[@]} > 0 )); then
  for c in "${candidates[@]}"; do
    size=$(byte_count "$c")
    total=$((total + size))
    printf '  %8s  %s\n' "$(fmt_kib "$size")" "${c#$TARGET_DIR/}"
  done
fi
if [[ ${#candidates[@]} -eq 0 ]]; then
  echo "  (none)"
fi
echo "Total removable: $(fmt_kib "$total")"
echo
echo "=== Retained (not touched) ==="
echo "  target/debug/, target/release/  (except incremental/) — dev & release builds stay warm"
echo "  dist/, build/, apple/KiemKit    — outside target/, never touched"
echo

if [[ "$APPLY" != 1 ]]; then
  echo "Dry run complete: 0 bytes deleted."
  exit 0
fi

# Guards 1 & 2 only apply to real deletion.
if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
  echo "error: cargo or rustc is running. Cleanup during a build can corrupt it — wait for the build to finish." >&2
  exit 1
fi

have_artifact=false
[[ -d "$ROOT_DIR/apple/KiemKit" ]] && have_artifact=true
[[ "$have_artifact" == false && -n "$(ls "$ROOT_DIR"/dist/Kiem-*.dmg 2>/dev/null)" ]] && have_artifact=true
[[ "$have_artifact" == false && -d "$ROOT_DIR/build/release/export/Kiem.app" ]] && have_artifact=true
if [[ "$have_artifact" == false ]]; then
  echo "error: no packaged artifact found (apple/KiemKit, dist/Kiem-*.dmg, or" >&2
  echo "       build/release/export/Kiem.app). These target/ trees are build inputs with" >&2
  echo "       no preserved output — deleting them now would just force a full rebuild." >&2
  exit 2
fi

freed=0
if (( ${#candidates[@]} > 0 )); then
  for c in "${candidates[@]}"; do
    size=$(byte_count "$c")
    echo "Removing ${c#$TARGET_DIR/} ($(fmt_kib "$size")) ..."
    rm -rf -- "$c"
    freed=$((freed + size))
  done
fi
echo "Done: reclaimed $(fmt_kib "$freed") from target/."