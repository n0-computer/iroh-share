# blobtorrent

A background service for sharing and downloading iroh blob collections. The daemon
owns one iroh endpoint and one filesystem blob store shared by all transfers. Its
authenticated QUIC iRPC control API supports starting, listing, watching, and
removing jobs.

The Cargo workspace has four crates:

- `blobtorrent-proto`: typed iRPC messages and state snapshots, with optional
  `client` helpers for native clients.
- `blobtorrent`: daemon and command-line client in one binary.
- `blobtorrent-tui`: Ratatui terminal interface using the same control protocol.
- `blobtorrent-gui`: egui desktop interface using the same control protocol.

```sh
cargo run -- --state-dir /path/to/state daemon
cargo run -- --state-dir /path/to/state share /path/to/directory
cargo run -- --state-dir /path/to/state download '<blob ticket>' /path/to/target
cargo run -- --state-dir /path/to/state list
cargo run -- --state-dir /path/to/state watch
cargo run -- --state-dir /path/to/state remove 0
```

Start the daemon, then open the desktop GUI:

```sh
cargo run -- daemon
# Paste the ticket printed by the daemon when prompted.
cargo run -p blobtorrent-gui
```

For the terminal interface, use `cargo run -p blobtorrent-tui`.

The TUI shows live states and progress. Seeding details start with the collection's
`https://<z32-hash>.blake3.net/` URL, followed by the hash and ticket; failed jobs
show their error. Use Up/Down (or j/k) to select an item, s to share a path,
d to enter a ticket and download target, x to remove the selected item (y confirms),
PageUp/PageDown to scroll details, and q or Ctrl-C to quit. Escape cancels a prompt.
In path prompts, Tab completes filenames and directories; repeat Tab to cycle
matches or Shift-Tab to cycle backwards. A unique directory completion adds a
separator so another Tab explores it. `~/` expands to the daemon user’s home directory, and
names containing spaces need no quoting. Hidden entries appear when you type a
leading dot. Relative paths use the daemon’s working directory. Completion runs on
the daemon through iRPC; the TUI does not inspect its local filesystem. The daemon keeps running when
the TUI exits. If the connection drops, the TUI reconnects and replaces its job list
with a fresh snapshot; commands are disabled until that snapshot is complete.

To install the daemon and either frontend:

```sh
cargo install --path blobtorrent
cargo install --path blobtorrent-gui
cargo install --path blobtorrent-tui
```

Without `--state-dir`, the daemon and CLI use this per-user location:

- Linux: `$XDG_STATE_HOME/blobtorrent`, or `~/.local/state/blobtorrent`.
- macOS: `~/Library/Application Support/blobtorrent`.
- Windows: `%LOCALAPPDATA%\blobtorrent`.

`--state-dir` overrides this location.

Use `daemon --no-announce` to disable Mainline announcements and name publication for local testing.
At startup, the daemon logs endpoint-indexer `host:port` advertisements from the
Mainline rendezvous hash at `info` level, deduplicating results over a lookup of up
to 30 seconds. These are advertised candidates, not verified live indexers. Empty,
failed, or timed-out lookups produce warnings. Logs go to stderr; set `RUST_LOG`
to change the default `warn,blobtorrent=info` filter. `--no-announce` skips discovery.

Normally, a shared Mainline publisher announces completed collections and retries
on discovery failures. Direct ticket transfers work independently of Mainline.
The announcement library is pinned to a Git revision in `Cargo.toml`.

The iRPC protocol lives in `blobtorrent-proto/src/lib.rs`. Hashes use `iroh_blobs::Hash`, and tickets
use `BlobTicket`. Ticket strings are parsed at the CLI boundary. The control server
shares the blob iroh endpoint on the `/blobtorrent/control/1` ALPN. It checks the
authenticated remote endpoint ID against a persistent allowlist before dispatching
RPC requests. Blob connections are public.

Each `Job` contains an ID, its original request (`JobKind`), and a `JobState`:

- `Queued`: accepted and waiting to start.
- `Importing { progress }`: known byte and file totals for source files.
- `Downloading { source, progress }`: source ticket and byte progress; total bytes
  may be unknown and include collection metadata.
- `Exporting { root_hash, progress }`: collection hash and payload byte/file counts.
- `Seeding { ticket }`: a required ticket containing the root hash (`ticket.hash()`).
- `Failed { error }`: a `JobError` containing the failure message.

State-specific fields live inside these variants. There is no separate phase or
optional top-level hash/ticket. Seeding continues until removal or daemon shutdown;
it does not imply that public Mainline publication has succeeded.

`Watch` streams `Result<WatchEvent, String>`. It first emits `JobUpdated` for each
existing job, `NameUpdated` for each name, and `GatewayUpdated`, then `SnapshotComplete` (also for an empty daemon). Subsequent events
are complete `JobUpdated`/`NameUpdated`/`GatewayUpdated` snapshots or `JobRemoved { id }`/`NameRemoved { label }`. Clients replace their
entry on an update and delete it on removal. Slow watchers are disconnected;
reconnect to obtain a fresh snapshot.

`CompletePath { path: PathBuf }` returns `PathCompletions`: a common prefix and
sorted `PathCandidate { path, kind }` entries, where `kind` is `File` or `Directory`.
Candidates are absolute daemon paths; directories include a trailing separator.
Responses contain at most 256 candidates and indicate truncation. Clients can refine
the prefix for large directories. Hidden names require a leading dot; names that
cannot be represented as UTF-8 or contain control characters are omitted.
Completion uses the same endpoint allowlist as other control requests. Directory
reads run outside the control actor, and the TUI discards responses for edited or
closed prompts. Share and Download interpret `~/` and relative paths on the daemon;
the local CLI resolves relative arguments in its working directory before sending them.

The shared store lives in `STATE_DIR/blobs`. Sharing references source files in
place. Downloads initially write into the store, then export with `TryReference`:
large payloads are moved to the target and referenced there for subsequent seeding.
The store retains metadata and outboards; small blobs may remain inline in its
database. Shared files and directories are rescanned every two seconds and reimported after a stable scan one second later. Keep downloaded files unchanged while seeding. Existing target
files, unsafe collection paths, and overlapping download targets are rejected.
Removing a job cancels its task and stops renewing its announcement, without
removing source or exported files. Data already in the shared store may still be
served by hash; removal is not a revocation or immediate garbage collection.

Names, signing keys, shares, and download requests are persistent, regardless of their current transfer state. Entries retain their IDs across restarts. Imports restart from scratch; incomplete downloads reuse local blob data; interrupted exports rerun; completed downloads resume seeding without export. The daemon endpoint identity is persistent. Names attached to removed data wait until retargeted.
Downloads currently use the provider in the ticket and do not schedule parallel
ranges of a single blob across multiple providers.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Gateway settings

Press **,** or **F2** to open Settings, or use Tab to cycle through Data, Names,
and Settings. The page configures the gateway embedded in the daemon:

- **Gateway enabled:** start automatically and keep running with the daemon.
- **HTTP listen address:** a loopback address, default `127.0.0.1:8080`.
- **Index server:** an optional IPv4 `host:port`; empty uses Mainline rendezvous discovery.

Use Up/Down to select, Space to toggle, and Enter to edit a field. Ctrl-U clears
an edit. Press **s** to save and apply, **r** to discard unsaved changes, or Esc
to return. Applying settings restarts the gateway. Its state is shown as Disabled,
Starting, Running, or Failed; failures retry every 30 seconds. Settings are saved
atomically in `STATE_DIR/gateway.json` and restored on daemon startup. The gateway
is enabled by default on `127.0.0.1:8080`. An explicitly saved disabled setting is preserved. Closing the TUI does not stop it.

The daemon embeds `iroh-local-gateway` as a library, with its own iroh endpoint and
Mainline resolver. It streams the content-addressed web through normal network
protocols and does not access the daemon's blob store directly. Its separate endpoint
can fetch content from the daemon like any other provider. Configure the companion
browser extension to use the gateway's HTTP address on the daemon machine.
`--no-announce` disables publication, independently of the gateway setting.

The control protocol exposes `GetGateway`, `SetGateway { config }`, and complete
`GatewayUpdated` snapshots through Watch. The snapshot includes the desired typed
configuration and runtime state. Only Running carries a bound HTTP address and
endpoint ID; only Failed carries an error.

## Pkarr names

Names provide a stable `https://<public-key>.pkarr.net/` URL pointing to an HTTP(S)
URL or an existing job's current `blake3.net` URL:

```sh
blobtorrent names create website --url 'https://example.com/path?query=value'
blobtorrent names create shared --job 0
blobtorrent names list
blobtorrent names update website --url 'https://example.org/new-path'
blobtorrent names remove website
```

Updating a name preserves its key and public URL. Job targets follow shared files
and directories automatically as their collection hash changes. While a changed
share imports, its last record continues to be published. Old snapshots are not
archived. A new name waits for its target job to finish importing or downloading.
Removing a job pauses its names and prevents automatic restart of that share.

In the TUI, Tab cycles through Data, Names, and Settings. Press n on a selected item to name
it; in Names, n creates a name, e edits its target, and x removes it after
confirmation. Targets accept a full HTTP(S) URL, `data:<id>`, or `job:<id>`.

The daemon signs DNS records and republishes them on Mainline every ten minutes,
retrying failures after thirty seconds. The Names view and Watch expose publication
status separately from transfer state. `--no-announce` keeps names locally with
publication disabled. Full URL targets use a URI record at `_https._tcp.<key>`;
origin-only HTTPS targets also include an apex HTTPS record for compatibility.
The local gateway understands both. Opening pkarr.net URLs uses the companion
browser extension and local gateway.

Keys, targets, signed records, and all data requests and recovery phases live in `STATE_DIR/names.json`
(mode 0600 on Unix). Back up this file to retain control of the public names.
Removing a name deletes its local key and stops republishing; cached DHT records
expire later. Names must fit the 1000-byte signed DNS packet limit, so very long
URLs are rejected. Credentials in URLs are not supported.

### Copying and opening URLs

Press **c** to copy the selected URL or **o** to open it in your default browser.
Data uses its `blake3.net` URL once seeding; Names uses the stable `pkarr.net`
URL. The heading also provides an **Open selected URL** terminal hyperlink
(usually Ctrl-click or Cmd-click, depending on the terminal).

Local copying uses the desktop clipboard. Over SSH, or if the desktop clipboard
is unavailable, copying requests the terminal clipboard using OSC 52. Your terminal
must support and permit it; tmux must permit clipboard integration. This protocol
does not confirm success, so the status says “Copy requested from terminal”.
Over SSH, **o** directs you to the terminal hyperlink or copying instead of
launching a browser on the remote host. Clipboard and browser operations run
outside the rendering loop.

### Persistent data

A share is saved atomically before the daemon accepts it. Removing it saves the
removal before stopping its task, so it stays removed after restart. Removing or
retargeting a name leaves its share intact.

On restart the daemon reimports each registered path; transient progress and
previous seeding state are not restored as if still current. Changes made while
the daemon was stopped are therefore picked up. Missing paths remain listed as
failed and are retried, allowing a disconnected drive to return. Original source
files must remain available; the blob store is not a backup of shared files.

Download recovery records distinguish Downloading, Exporting, and Seeding. The
Exporting checkpoint is committed before exporting any file; Seeding is committed
after all exports finish. Failures keep the request and its last recovery phase,
so restart retries that work. Progress counters are reconstructed rather than
written on every update. Persistent blob tags retain downloaded data across restart.

Only recovery in Exporting reruns export. With published `iroh-blobs`, the application
cannot inspect external reference paths, so blobtorrent hashes existing target files
and skips them when they match the expected blob. Different files are rejected.
This adds a verification read when recovering an interrupted export. Completed
seeders do not run export again. Keep downloaded files unchanged while seeding,
as the store references them.

Blobtorrent uses the published `iroh-blobs` crate.

### Control identities and remote TUI setup

The daemon stores its stable identity in `STATE_DIR/daemon.key`. The CLI uses
`STATE_DIR/control-client.key`, which the daemon automatically authorizes.
The TUI has its own identity and configuration and never reads the daemon's state
directory. Its default directory is selected using `dirs::config_dir()`:

- Linux: `$XDG_CONFIG_HOME/blobtorrent-tui`, or `~/.config/blobtorrent-tui`.
- macOS: `~/Library/Application Support/blobtorrent-tui`.
- Windows: `%APPDATA%\blobtorrent-tui`.

Use `--config-dir` to override it. It contains `control-client.key` and `client.json`
(the saved daemon identity and address hints). Keys are created atomically with mode
0600 on Unix. The pairing secret is not saved by the TUI.

On its first startup, `blobtorrent daemon` prints a ready-to-run command:

```sh
blobtorrent-tui '<pairing-ticket>'
```

The ticket contains the daemon identity, address hints, and a random one-time secret.
It uses `iroh-tickets` with the `blobtorrent` prefix and a versioned postcard payload.
You can also launch `blobtorrent-tui` without arguments and paste the ticket at its
first-run prompt. The TUI creates its own persistent identity, redeems the ticket,
saves the connection, and opens the interface without another setup step.
Subsequent launches only need `blobtorrent-tui`.
To create additional tickets while the daemon is running:

```sh
blobtorrent control pair
```

Multiple tickets can be outstanding at once. Each authorizes one authenticated
client identity; retries by that same client are safe, and a ticket cannot restore
revoked access. Tickets are valid until redeemed or the daemon stops. Paired client
authorizations persist across daemon restarts. Anyone holding an unused ticket can
claim its full control access.

Pairing uses `/blobtorrent/pair/1` on the same iroh endpoint. It exposes only enrollment;
normal control requests use the endpoint allowlist. Authorization is saved before
success is reported. The disconnected TUI displays ticket setup instructions.
Path completion and resolution use the daemon’s filesystem, so the TUI can run on
another machine. `--endpoint <daemon-id>` selects an explicitly authorized daemon
without redeeming a ticket; `--print-id` prints the TUI's identity.

`blobtorrent control list` lists allowed IDs; `blobtorrent control revoke <id>`
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

## Desktop GUI

Run `cargo run -p blobtorrent-gui --release`. Paste the daemon's one-time ticket
into the connection screen, or pass it as a positional argument. The GUI keeps
its own identity and saved daemon address in the platform configuration directory
under `blobtorrent-gui`; `--config-dir` overrides it.

Data, Names, and Settings provide sharing, downloads, pkarr management, and gateway
configuration. Content links can be opened or copied; blob tickets are secondary.
Use **Complete** for paths on the daemon. When both apps share a filesystem,
enable **The daemon is on this computer** in Settings to choose folders, share
files/folders by dropping them into the window, and open seeded paths in the
system file manager. Closing the GUI leaves the daemon and gateway running.

Frontend capabilities and protocol behavior are described in
[the UI capability guide](blobtorrent-proto/UI.md). Frontends can evolve independently.

## CI and binary releases

GitHub Actions runs formatting, Clippy, and workspace tests on pull requests and
pushes to `main`. The **Binary releases** workflow builds the daemon, TUI, and GUI
for Windows x64 and macOS Apple Silicon. It can be run manually to
produce downloadable workflow artifacts without creating a release.

Pushing a `v*` tag publishes the archives and SHA-256 checksums as a GitHub Release
once both builds succeed. Windows uses ZIP; macOS uses tar.gz and also
includes a `Blobtorrent.app` bundle. These builds are unsigned and not notarized;
OS download protections may require explicit approval to run them.

## Windows per-user installer

Run `blobtorrent-<version>-windows-x64-setup.exe` from the release. Setup installs
under `%LOCALAPPDATA%\Programs\Blobtorrent`, adds Start menu shortcuts, and starts
the daemon in the background for the current account. It registers the daemon to
start at login; administrator access is not required. This is a per-user login
process, not a Windows system service, and does not run before login.

A new desktop configuration is paired automatically using a one-time invitation.
The GUI keeps its own endpoint identity. An existing GUI connection, including a
remote daemon, is preserved. Local folder integration is enabled for the
installer-paired daemon. Closing the GUI leaves the daemon running.

The Start menu includes **Start background daemon** and **Stop background daemon**.
Windows **Settings → Apps → Startup** controls whether it starts at login. The
helper `blobtorrent-background.exe --setup-gui` retries initial setup;
`blobtorrent-background.exe stop` stops the local daemon gracefully. Logs are
`%LOCALAPPDATA%\blobtorrent\daemon.log` and `launcher.log`. Daemon output rotates
to `daemon.previous.log` on startup once the current log exceeds 5 MiB.

Upgrades stop the daemon before replacing its binaries and start it afterwards.
Uninstall stops it and removes the installed files and login entry. Saved daemon
state, GUI identity/settings, and shared/downloaded files remain on disk. The
installer is unsigned, like the standalone binaries.

The release workflow builds the installer with Inno Setup and tests silent
installation, local pairing, sharing, upgrade, uninstall, and preservation of
user data on a Windows runner. Portable ZIP archives remain available.
