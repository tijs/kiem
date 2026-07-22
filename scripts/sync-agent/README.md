# Unattended sync on a headless Mac

Keeps `kiem sync` running as a per-user `launchd` LaunchAgent, so a Mac with
no one logged in interactively (only an agent driving the `kiem` CLI) stays
synced without the GUI app open. macOS only — a `LaunchAgent`, not a
`LaunchDaemon`, so it still needs the user logged in (enable auto-login on
this Mac if it should survive a reboot with zero physical interaction).

## One-time setup

1. Pair this device first, if it isn't already:

   ```bash
   kiem pair add <ticket>
   ```

2. Install and start the agent:

   ```bash
   scripts/sync-agent/install.sh
   ```

   If `Kiem.app` is currently open, `install.sh` will warn and ask to quit it
   first — **never run the GUI app and this LaunchAgent against the same data
   dir at the same time.** The GUI's sync doesn't share the daemon's
   control-socket lock, so two meshes on one identity corrupt discovery. Pass
   `--yes` to skip the confirmation (e.g. in a non-interactive setup script).

## Checking health

```bash
kiem sync-status
tail -f ~/Library/Logs/kiem-sync.log
```

## Uninstalling

```bash
scripts/sync-agent/uninstall.sh
```

Stops the agent and removes `~/Library/LaunchAgents/org.tijs.kiem.sync.plist`.
