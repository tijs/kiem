#!/usr/bin/env bash
set -euo pipefail

# Installs org.tijs.kiem.sync as a per-user LaunchAgent so `kiem sync` stays
# alive unattended (headless-Mac support). See README.md in this directory.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LABEL="org.tijs.kiem.sync"
DATA_DIR="${DATA_DIR:-$HOME/.kiem}"
LOG_PATH="$HOME/Library/Logs/kiem-sync.log"
PLIST_PATH="$HOME/Library/LaunchAgents/$LABEL.plist"
UID_GID="gui/$(id -u)"

ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    --yes) ASSUME_YES=1 ;;
    *)
      echo "usage: install.sh [--yes]" >&2
      exit 1
      ;;
  esac
done

KIEM_BIN="$(command -v kiem || true)"
if [[ -z "$KIEM_BIN" ]]; then
  echo "install.sh: 'kiem' not found on PATH — install the app or add its CLI to PATH first" >&2
  exit 1
fi

# The GUI app's sync start doesn't share the daemon's control-socket lock, so
# two meshes on one identity would corrupt discovery (see plan for this unit
# in the Kiem project notes). Refuse to run both against the same data dir.
GUI_PID="$(pgrep -f "Kiem\.app/Contents/MacOS/Kiem$" || true)"
if [[ -n "$GUI_PID" ]]; then
  echo "install.sh: the Kiem.app GUI is running (pid $GUI_PID) and syncing the same identity." >&2
  echo "Running both the GUI app and this LaunchAgent at once corrupts sync discovery." >&2
  if [[ "$ASSUME_YES" -eq 0 ]]; then
    read -r -p "Quit Kiem.app now and continue installing? [y/N] " reply
    if [[ ! "$reply" =~ ^[Yy]$ ]]; then
      echo "install.sh: aborted — quit Kiem.app yourself, then re-run." >&2
      exit 1
    fi
  fi
  kill "$GUI_PID"
  sleep 1
fi

mkdir -p "$(dirname "$PLIST_PATH")" "$(dirname "$LOG_PATH")"
sed \
  -e "s|__KIEM_BIN__|$KIEM_BIN|g" \
  -e "s|__DATA_DIR__|$DATA_DIR|g" \
  -e "s|__LOG_PATH__|$LOG_PATH|g" \
  "$ROOT_DIR/scripts/sync-agent/org.tijs.kiem.sync.plist.template" > "$PLIST_PATH"

launchctl bootout "$UID_GID/$LABEL" 2>/dev/null || true
launchctl bootstrap "$UID_GID" "$PLIST_PATH"
launchctl enable "$UID_GID/$LABEL"
launchctl kickstart -k "$UID_GID/$LABEL"

echo "install.sh: $LABEL loaded and started. Check with: kiem sync-status"
