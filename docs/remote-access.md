# Pairing and remote control

The daemon stores its stable identity in `STATE_DIR/daemon.key`. The CLI uses
`STATE_DIR/control-client.key`, which the daemon automatically authorizes.
The TUI has its own identity and configuration and never reads the daemon's state
directory. Its default directory is selected using `dirs::config_dir()`:

- Linux: `$XDG_CONFIG_HOME/iroh-share-tui`, or `~/.config/iroh-share-tui`.
- macOS: `~/Library/Application Support/iroh-share-tui`.
- Windows: `%APPDATA%\iroh-share-tui`.

Use `--config-dir` to override it. It contains `control-client.key` and `client.json`
(every paired daemon with its address hints and optional name, plus the one used
most recently). Keys are created atomically with mode 0600 on Unix. The pairing
secret is not saved by the TUI. One client identity is used for all daemons.

On its first startup, `iroh-share daemon` prints a ready-to-run command:

```sh
iroh-share-tui '<pairing-ticket>'
```

The ticket contains the daemon identity, address hints, and a random one-time secret.
It uses `iroh-tickets` with the `iroh-share` prefix and a versioned postcard payload.
You can also launch `iroh-share-tui` without arguments and paste the ticket at its
first-run prompt. The TUI creates its own persistent identity, redeems the ticket,
saves the connection, and opens the interface without another setup step.
Subsequent launches only need `iroh-share-tui`; it reconnects to the daemon used
most recently.

The TUI can manage several daemons. Press **m** to open the Daemons page: Enter
switches to the selected daemon, **a** adds one by pasting its pairing ticket,
**n** names it, and **x** forgets it. Forgetting only removes the daemon from this
list; it keeps the TUI authorized until revoked with `iroh-share control revoke`.
The heading shows which daemon is current.

To create additional tickets while the daemon is running:

```sh
iroh-share control pair
```

Multiple tickets can be outstanding at once. Each authorizes one authenticated
client identity; retries by that same client are safe, and a ticket cannot restore
revoked access. Tickets are valid until redeemed or the daemon stops. Paired client
authorizations persist across daemon restarts. Anyone holding an unused ticket can
claim its full control access.

Pairing uses `/iroh-share/pair/1` on the same iroh endpoint. It exposes only enrollment;
normal control requests use the endpoint allowlist. Authorization is saved before
success is reported. The disconnected TUI displays ticket setup instructions.
Path completion and resolution use the daemon’s filesystem, so the TUI can run on
another machine. `--endpoint <daemon-id>` selects an explicitly authorized daemon
without redeeming a ticket; `--print-id` prints the TUI's identity.

`iroh-share control list` lists allowed IDs; `iroh-share control revoke <id>`
removes one and closes its active control connections, including Watch streams.
The implicit local owner cannot be revoked through RPC. Allowed clients currently
have full control, including allowlist management. Grants are stored in
`control-allowed.json` before being applied. Invalid allowlist files fail closed.

An embedded iroh client can use the exported `CONTROL_ALPN` and `ControlProtocol`
with `irpc_iroh::client`, using its own persistent key and the saved daemon ID.
The native client helper also exposes `ControlClient::from_endpoint` for a
caller-owned endpoint. The allowlist boundary sits at the authenticated connection,
independent of CLI/TUI or browser transport; no browser UI is included yet.

The daemon writes `control.addr` for local CLI discovery.
