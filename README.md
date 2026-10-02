# iroh-share

## Publishing your content

The GUI's Data pane supports local file/folder drops and **Import ticket**
for publishing to a remote daemon. Give content an optional name for a stable
pkarr URL. Content names are managed alongside the data; standalone URL names and
ordinary downloads are in collapsed sections.

For an existing item, **Update from ticket** imports a new version while retaining
its names. A named local directory also offers **Refresh directory**. Keep the
sendme sender running until a ticket import completes. Paste a ticket or drop a
`.ticket`/`.sendme` file; dropping a local folder requires the same-filesystem setting.

Ticket imports are exported into `Downloads/Iroh Share` on the daemon's machine,
separate from private daemon state and the blob store. Each version has its own
folder, and previous files are kept. Override the default with:

```sh
iroh-share daemon --import-dir /path/to/published-content
```

The GUI shows the configured import directory and each item's exported path.
See [the publishing UX plan](iroh-share-proto/PUBLISHING-UX.md) and
[UI capabilities](iroh-share-proto/UI.md) for protocol details.

A background service for sharing and downloading iroh blob collections. The daemon
owns one iroh endpoint and one filesystem blob store shared by all transfers. Its
authenticated QUIC iRPC control API supports starting, listing, watching, and
removing jobs.

The Cargo workspace has four crates:

- `iroh-share-proto`: typed iRPC messages and state snapshots, with optional
  `client` helpers for native clients.
- `iroh-share`: daemon and command-line client in one binary.
- `iroh-share-tui`: Ratatui terminal interface using the same control protocol.
- `iroh-share-gui`: egui desktop interface using the same control protocol.

```sh
cargo run -- --state-dir /path/to/state daemon
cargo run -- --state-dir /path/to/state share /path/to/directory
cargo run -- --state-dir /path/to/state download 'https://<z32-hash>.blake3.net/' /path/to/target
cargo run -- --state-dir /path/to/state list
cargo run -- --state-dir /path/to/state watch
cargo run -- --state-dir /path/to/state remove 0
```

Start the daemon, then open the desktop GUI:

```sh
cargo run -- daemon
# Paste the ticket printed by the daemon when prompted.
cargo run -p iroh-share-gui
```

For the terminal interface, use `cargo run -p iroh-share-tui`.

The TUI shows live states and progress. Seeding details start with the collection's
`https://<z32-hash>.blake3.net/` URL, followed by the hash and ticket; failed jobs
show their error. Use Up/Down (or j/k) to select an item, s to share a path,
d to enter a URL, hash, or ticket and download target, x to remove the selected item (y confirms),
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
cargo install --path iroh-share
cargo install --path iroh-share-gui
cargo install --path iroh-share-tui
```

Releases also include prebuilt Linux binaries for x86_64 and aarch64
(`iroh-share-<arch>-unknown-linux-musl.tar.gz`): `iroh-share`,
`iroh-share-background` and `iroh-share-tui`, statically linked. They run on
any Linux, including NAS systems with an old glibc. The GUI is not included;
build it with `cargo install --path iroh-share-gui`.

For a Linux user service that starts automatically, see the
[systemd example](packaging/systemd/README.md).

Without `--state-dir`, the daemon and CLI use this per-user location:

- Linux: `$XDG_STATE_HOME/iroh-share`, or `~/.local/state/iroh-share`.
- macOS: `~/Library/Application Support/iroh-share`.
- Windows: `%LOCALAPPDATA%\iroh-share`.

`--state-dir` overrides this location.

Use `daemon --no-announce` to disable Mainline announcements and name publication for local testing.
Endpoint indexers come from the signed server list maintained by n0, a Pkarr
record resolved through Mainline. Logs go to stderr; set `RUST_LOG` to change the
default `warn,iroh_share=info` filter. `--no-announce` skips discovery.

Normally, a shared Mainline publisher announces completed collections and retries
on discovery failures. Direct ticket transfers work independently of Mainline.
The announcement library is pinned to a Git revision in `Cargo.toml`.

The iRPC protocol lives in `iroh-share-proto/src/lib.rs`. Hashes use `iroh_blobs::Hash`, and tickets
use `BlobTicket`. Download inputs are parsed by clients using `DownloadSource::from_str`
from the protocol crate. The control server
shares the blob iroh endpoint on the `/iroh-share/control/5` ALPN. It checks the
authenticated remote endpoint ID against a persistent allowlist before dispatching
RPC requests. Blob connections are public.

Each `Job` contains an ID, its original request (`JobKind`), and a `JobState`:

- `Queued`: accepted and waiting to start.
- `Importing { progress }`: known byte and file totals for source files.
- `Downloading { source, progress }`: hash, provider addresses, discovery policy, and byte progress; total bytes
  may be unknown and include collection metadata.
- `Exporting { root_hash, progress }`: collection hash and payload byte/file counts.
- `Seeding { ticket, active_uploads }`: a required ticket containing the root hash (`ticket.hash()`).
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

Seeding snapshots include `active_uploads`, the number of active requests for the
collection root. Clients can show this beside Seeding. Counts reset on restart
and drop when requests complete, fail, or disconnect. Individual child-blob
requests are not attributed to collections; counts describe requests, not people.

Names, signing keys, shares, and download requests are persistent, regardless of their current transfer state. Entries retain their IDs across restarts. Imports restart from scratch; incomplete downloads reuse local blob data; interrupted exports rerun; completed downloads resume seeding without export. The daemon endpoint identity is persistent. Names attached to removed data wait until retargeted.
Downloads accept collection root URLs (`https://<z32-hash>.blake3.net/`), bare
z32 or hexadecimal BLAKE3 hashes, and collection tickets. URLs with subpaths,
queries, or fragments are rejected; downloads export whole collections.
Clients parse the input into `DownloadSource` and send one `Download` RPC.
The source contains a BLAKE3 hash, provider addresses (if known), and a discovery
policy. `Disabled` restricts downloads to supplied providers; `Mainline` permits
finding additional peers if the supplied providers cannot finish. Downloads
retain verified partial data between providers. Discovery has a 60-second
deadline. URL/hash inputs enable discovery. Sendme-compatible collection tickets
preserve their address hints and disable discovery by default; use `--discover`
in the CLI or “Also discover providers for tickets” in the GUI to enable it.
Complete local collections can be exported without discovery. Transfers do not
schedule parallel ranges of a single blob across multiple providers.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Gateway settings

Press **,** or **F2** to open Settings, or use Tab to cycle through Data, content names,
and Settings. The page configures the gateway embedded in the daemon:

- **Gateway enabled:** start automatically and keep running with the daemon.
- **HTTP listen address:** a loopback address, default `127.0.0.1:45475`.
- **Index server:** an optional IPv4 `host:port`; empty uses the index servers listed by n0.

Use Up/Down to select, Space to toggle, and Enter to edit a field. Ctrl-U clears
an edit. Press **s** to save and apply, **r** to discard unsaved changes, or Esc
to return. Applying settings restarts the gateway. Its state is shown as Disabled,
Starting, Running, or Failed; failures retry every 30 seconds. Settings are saved
atomically in `STATE_DIR/gateway.json` and restored on daemon startup. The gateway
is enabled by default on `127.0.0.1:45475`. An explicitly saved disabled setting is preserved. Closing the TUI does not stop it.

The daemon embeds `iroh-link-gateway` as a library, with its own iroh endpoint and
Mainline resolver. It streams the content-addressed web through normal network
protocols and does not access the daemon's blob store directly. Its separate endpoint
can fetch content from the daemon like any other provider. Configure the companion
[browser extension][iroh-link] to use the gateway's HTTP address on the daemon machine.
`--no-announce` disables publication, independently of the gateway setting.

The control protocol exposes `GetGateway`, `SetGateway { config }`, and complete
`GatewayUpdated` snapshots through Watch. The snapshot includes the desired typed
configuration and runtime state. Only Running carries a bound HTTP address and
endpoint ID; only Failed carries an error.

## Pkarr names

Names provide a stable `https://<public-key>.pkarr.net/` URL pointing to an HTTP(S)
URL or an existing job's current `blake3.net` URL:

```sh
iroh-share names create website --url 'https://example.com/path?query=value'
iroh-share names create shared --job 0
iroh-share names list
iroh-share names update website --url 'https://example.org/new-path'
iroh-share names remove website
```

Updating a name preserves its key and public URL. Job targets follow shared files
and directories automatically as their collection hash changes. While a changed
share imports, its last record continues to be published. Old snapshots are not
archived. A new name waits for its target job to finish importing or downloading.
Removing a job pauses its names and prevents automatic restart of that share.

In the TUI, Tab cycles through Data, its content-name view, and Settings. Press n
on selected data to name it. In the content-name view, n creates a name, e edits
its target, and x removes it after confirmation. Retarget linked names by selecting
a path with Up/Down; in a URL prompt, Ctrl+D switches to data selection. N expands
standalone URL names. D expands downloads, where d opens the download prompt.
Use i for a new sendme ticket import, u to update selected data from a ticket,
and r to refresh a named local directory.

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
Named data uses its stable `pkarr.net` URL; unnamed data uses its `blake3.net`
URL once seeding. The content-name and standalone-name views use `pkarr.net` URLs. The heading also provides an **Open selected URL** terminal hyperlink
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
cannot inspect external reference paths, so iroh-share hashes existing target files
and skips them when they match the expected blob. Different files are rejected.
This adds a verification read when recovering an interrupted export. Completed
seeders do not run export again. Keep downloaded files unchanged while seeding,
as the store references them.

Iroh Share uses the published `iroh-blobs` crate.

### Control identities and remote TUI setup

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

## Desktop GUI

Run `cargo run -p iroh-share-gui --release`. Paste the daemon's one-time ticket
into the connection screen, or pass it as a positional argument. The GUI keeps
its own identity and saved daemons in the platform configuration directory
under `iroh-share-gui`; `--config-dir` overrides it.

The GUI can manage several daemons. The **Daemon** menu at the top switches
between saved daemons and adds another with its pairing ticket and an optional
name. On start, the GUI reconnects to the daemon used most recently. Settings can
rename the current daemon or forget it; forgetting removes it from this list only.

Data is the publishing screen, with content-linked names alongside it. Standalone
URL names and ordinary downloads are collapsed sections. Settings contains gateway
configuration. Public links and sendme tickets have copy actions.
Use **Complete** for paths on the daemon. When both apps share a filesystem,
enable **The daemon is on this computer** in Settings to choose folders, share
files/folders by dropping them into the window, and open seeded paths in the
system file manager. This setting is saved separately for each daemon. Closing the GUI leaves the daemon and gateway running.

Frontend capabilities and protocol behavior are described in
[the UI capability guide](iroh-share-proto/UI.md) and
[shared UX foundations](iroh-share-proto/UX.md). Layouts may differ; workflows agree.

## CI and binary releases

GitHub Actions runs formatting, Clippy, and workspace tests on pull requests and
pushes to `main`. The **Binary releases** workflow builds the daemon, TUI, and GUI
for Windows x64 and macOS Apple Silicon. It can be run manually to
produce downloadable workflow artifacts without creating a release.

Pushing a `v*` tag publishes the archives and SHA-256 checksums as a GitHub Release
once both builds succeed. Windows uses ZIP; macOS uses tar.gz and also
includes a `Iroh Share.app` bundle. These builds are unsigned and not notarized;
OS download protections may require explicit approval to run them.

## Windows per-user installer

Run `iroh-share-<version>-windows-x64-setup.exe` from the release. Setup installs
under `%LOCALAPPDATA%\Programs\Iroh Share`, adds Start menu shortcuts, and starts
the daemon in the background for the current account. It registers the daemon to
start at login; administrator access is not required. This is a per-user login
process, not a Windows system service, and does not run before login.

A new desktop configuration is paired automatically using a one-time invitation.
The GUI keeps its own endpoint identity. An existing GUI connection, including a
remote daemon, is preserved. Local folder integration is enabled for the
installer-paired daemon. Closing the GUI leaves the daemon running.

The Start menu includes **Start background daemon** and **Stop background daemon**.
Windows **Settings → Apps → Startup** controls whether it starts at login. The
helper `iroh-share-background.exe --setup-gui` retries initial setup;
`iroh-share-background.exe stop` stops the local daemon gracefully. Logs are
`%LOCALAPPDATA%\iroh-share\daemon.log` and `launcher.log`. Daemon output rotates
to `daemon.previous.log` on startup once the current log exceeds 5 MiB.

Upgrades stop the daemon before replacing its binaries and start it afterwards.
Uninstall stops it and removes the installed files and login entry. Saved daemon
state, GUI identity/settings, and shared/downloaded files remain on disk. The
installer is unsigned, like the standalone binaries.

The release workflow builds the installer with Inno Setup and tests silent
installation, local pairing, sharing, upgrade, uninstall, and preservation of
user data on a Windows runner. Portable ZIP archives remain available.

## macOS per-user installer

On Apple Silicon with macOS 13 or later, open
`iroh-share-<version>-macos-arm64.pkg`. It installs `Iroh Share.app` in
`~/Applications`, registers a per-user LaunchAgent, starts the daemon, and pairs a
new GUI configuration. Install for your own account, without `sudo`. Existing GUI
connections are preserved.

The daemon starts at login and stays running after the GUI closes. launchd
restarts failed daemon exits; an explicit graceful stop leaves it stopped until
login or restart. The daemon handles SIGTERM for logout and service shutdown.
The LaunchAgent is `~/Library/LaunchAgents/computer.n0.iroh-share.plist`; state
and logs live in `~/Library/Application Support/iroh-share`.

macOS can ask you to approve background activity and access to protected folders.
Installation does not bypass Downloads/Documents/Desktop privacy permissions.
The app has an ad-hoc signature for bundle integrity; the package is not Developer
ID signed or notarized, so this build still requires explicit approval under
macOS download protections.

To remove it, close the GUI and run `~/Applications/Uninstall Iroh Share.command`.
It stops and unregisters the LaunchAgent, then removes the app and uninstaller.
Saved state, GUI identity/settings, and shared/downloaded files are preserved.

Directory shares expose their contents at the collection root by default. Use
`iroh-share share --include-directory-name /path/to/directory` to include the
directory basename. The GUI has an Include directory name checkbox; Ctrl+R
toggles it in the TUI share prompt. The choice persists across refreshes and
restarts. Existing saved shares retain their layout.

Export all pkarr names, including those attached to content, from the Names
panel's **Export all pkarr names…** action, or run:

```sh
iroh-share names export pkarr-names.zip
```

The ZIP is saved on the client computer. Files are named by the z-base-32 public
key: `<key>.key` is the raw 32-byte Ed25519 signing seed, the same format as the
gateway's `--key-file`, and `<key>.pkarr` is the current signed packet, if one
exists. Aliases are not included. `.pkarr` files use pkarr's
`SignedPacket::as_bytes` format, which is also how the gateway stores signed
packets (`index-list.pkarr`). The ZIP contains unencrypted private keys; export
creates a new file without overwriting an existing file, with owner-only
permissions on Unix.

To back up a single name, use **Export pkarr…** in that name's Actions, or run:

```sh
iroh-share names export-name <label> <key>.zip
```

This writes the same ZIP layout with just that name.

Restore names with **Import pkarr names…** in the Names panel, or run:

```sh
iroh-share names import pkarr-names.zip
```

The whole archive is verified before anything is added: every key must match
its file name, and every record must carry a valid signature for its key. Keys
the daemon already manages are skipped, never overwritten. Imported names get
labels like `imported-<key prefix>` and become advanced DNS names holding the
imported records, which are republished unchanged until you edit them. A name
that followed data on the old machine no longer follows anything. A key without
a record is imported with no records and shows **No records yet**; it publishes
once you add records.

The Names panel also provides an advanced multiline DNS editor. Its default is
an HTTPS alias record:

```dns
@ 300 IN HTTPS 0 example.com.
```

Replace or add records using one `owner TTL IN TYPE value` per line, for example
`@ 300 IN A 192.0.2.1` or `@ 300 IN TXT "hello"`. The total DNS packet must fit
within 1000 bytes. Content-bound names generate their records automatically.

The bundled gateway resolves the apex HTTPS record. URI-only records, including
full-URL redirects with a path or query, are not supported by this gateway.

## License

Copyright 2026 N0, INC.

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

[iroh-link]: https://chromewebstore.google.com/detail/iroh-link/aajlbmaphckgbinhnifpiggcmdfnofcd
