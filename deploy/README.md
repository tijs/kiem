# Deploying the Kiem sync daemon on a home server

The always-on CLI peer is the primary non-Apple surface: it keeps every other
device converged and gives AI agents a sync-participating notes store.

## Install (Linux, per-user service)

```bash
# Build and install the binary
cargo install --path crates/kiem-cli   # installs ~/.cargo/bin/kiem
mkdir -p ~/.local/bin && ln -sf ~/.cargo/bin/kiem ~/.local/bin/kiem

# Install and start the service
mkdir -p ~/.config/systemd/user
cp deploy/kiem-sync.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now kiem-sync

# Survive logout (run user services at boot)
sudo loginctl enable-linger "$USER"
```

Check it:

```bash
systemctl --user status kiem-sync
kiem sync-status
journalctl --user -u kiem-sync -f
```

Notes live in `~/.kiem`. The fixed `--listen` port (7464) is optional — mDNS
advertises whatever port the daemon binds — but a stable port makes
firewalling and direct `--connect` setups (future cross-network peers)
simpler. Peers on the same LAN discover the daemon via mDNS automatically;
nothing else to configure.

For development, run the daemon in the foreground instead: `kiem sync`.
