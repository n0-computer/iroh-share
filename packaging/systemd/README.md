# Run iroh-share with systemd

This example runs the daemon as a Linux user service. It uses your normal
per-user state directory and can read shared files and write downloads with your
user's permissions. Relative daemon paths start in your home directory.

From the repository root:

```sh
cargo install --locked --path iroh-share --bin iroh-share
mkdir -p ~/.config/systemd/user
install -m 0644 packaging/systemd/iroh-share.service ~/.config/systemd/user/iroh-share.service
systemd-analyze --user verify ~/.config/systemd/user/iroh-share.service
systemctl --user daemon-reload
systemctl --user enable --now iroh-share.service
```

The unit expects the binary at `~/.cargo/bin/iroh-share`. Adjust `ExecStart` if
using a custom Cargo installation directory. Stop any manually started daemon
using the same state directory before starting the service.

To start at boot and keep running after logout, enable lingering for your user:

```sh
sudo loginctl enable-linger "$USER"
```

Without lingering, the service follows the lifetime of your user manager.

## Use and inspect

The CLI uses the same default state directory as the service:

```sh
iroh-share list
iroh-share share /absolute/path/to/directory
iroh-share control pair
systemctl --user status iroh-share.service
journalctl --user -u iroh-share.service -f
```

`control pair` creates a one-client invitation for a frontend. The service
suppresses the automatic first-start invitation so it is not written to the
journal. Ticket imports default to `~/Downloads/Iroh Share`; the embedded gateway
uses its saved configuration, initially `127.0.0.1:45475`.

To customize paths or logging, run `systemctl --user edit iroh-share.service`:

```ini
[Service]
ExecStart=
ExecStart=%h/.cargo/bin/iroh-share --state-dir %h/.local/state/iroh-share daemon --no-pairing-ticket --import-dir %h/Published
Environment=RUST_LOG=info
```

Then run `systemctl --user restart iroh-share.service`. If you change the state
directory, pass the same `--state-dir` to CLI commands. The service inherits the
user manager's environment; shell-only environment changes are not inherited.

## Upgrade or stop

Stop the service before replacing its binary:

```sh
systemctl --user stop iroh-share.service
cargo install --locked --path iroh-share --bin iroh-share
systemctl --user start iroh-share.service
```

To stop it and disable automatic startup:

```sh
systemctl --user disable --now iroh-share.service
```

Stopping or disabling the service preserves its state and shared files.
