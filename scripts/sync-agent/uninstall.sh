#!/usr/bin/env bash
set -euo pipefail

LABEL="org.tijs.kiem.sync"
PLIST_PATH="$HOME/Library/LaunchAgents/$LABEL.plist"
UID_GID="gui/$(id -u)"

launchctl bootout "$UID_GID/$LABEL" 2>/dev/null || true
rm -f "$PLIST_PATH"

echo "uninstall.sh: $LABEL stopped and removed."
