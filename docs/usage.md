# Using iroh-share

How to run the daemon and its frontends, publish and download content, and manage names. For pairing frontends with a daemon on another machine, see [pairing and remote control](remote-access.md).

## Running the daemon

```sh
cargo run -- --state-dir /path/to/state daemon
cargo run -- --state-dir /path/to/state share /path/to/directory
cargo run -- --state-dir /path/to/state download 'https://<z32-hash>.blake3.net/' /path/to/target
cargo run -- --state-dir /path/to/state list
cargo run -- --state-dir /path/to/state watch
cargo run -- --state-dir /path/to/state remove 0
```

Without `--state-dir`, the daemon and CLI use this per-user location:

- Linux: `$XDG_STATE_HOME/iroh-share`, or `~/.local/state/iroh-share`.
- macOS: `~/Library/Application Support/iroh-share`.
- Windows: `%LOCALAPPDATA%\iroh-share`.

`--state-dir` overrides this location.

Use `daemon --no-announce` to disable Mainline announcements and name publication for local testing.
Endpoint indexers come from the signed server list maintained by n0, a Pkarr
record resolved through Mainline. Logs go to stderr; set `RUST_LOG` to change the
default `warn,iroh_share=info` filter. `--no-announce` skips discovery.

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
URL names and ordinary downloads are collapsed sections. Public links and sendme tickets have copy actions.
Use **Complete** for paths on the daemon. When both apps share a filesystem,
enable **The daemon is on this computer** in Settings to choose folders, share
files/folders by dropping them into the window, and open seeded paths in the
system file manager. This setting is saved separately for each daemon. Closing the GUI leaves the daemon running.

Frontend capabilities and protocol behavior are described in
[the UI capability guide](../iroh-share-proto/UI.md) and
[shared UX foundations](../iroh-share-proto/UX.md). Layouts may differ; workflows agree.

## Terminal UI

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

## Publishing

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
See [the publishing UX plan](../iroh-share-proto/PUBLISHING-UX.md) and
[UI capabilities](../iroh-share-proto/UI.md) for protocol details.

Directory shares expose their contents at the collection root by default. Use
`iroh-share share --include-directory-name /path/to/directory` to include the
directory basename. The GUI has an Include directory name checkbox; Ctrl+R
toggles it in the TUI share prompt. The choice persists across refreshes and
restarts. Existing saved shares retain their layout.

## Downloading

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

## Names

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

In the TUI, Tab switches between Data and its content-name view. Press n
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
Iroh Link Gateway follows the apex HTTPS record, not URI-only records such as
full-URL redirects with a path or query.

Keys, targets, signed records, and all data requests and recovery phases live in `STATE_DIR/names.json`
(mode 0600 on Unix). Back up this file to retain control of the public names.
Removing a name deletes its local key and stops republishing; cached DHT records
expire later. Names must fit the 1000-byte signed DNS packet limit, so very long
URLs are rejected. Credentials in URLs are not supported.

### Advanced DNS records

The Names panel also provides an advanced multiline DNS editor. Its default is
an HTTPS alias record:


```dns
@ 300 IN HTTPS 0 example.com.
```


Replace or add records using one `owner TTL IN TYPE value` per line, for example
`@ 300 IN A 192.0.2.1` or `@ 300 IN TXT "hello"`. The total DNS packet must fit
within 1000 bytes. Content-bound names generate their records automatically.

### Backup and restore

Export all pkarr names, including those attached to content, from the Names
panel's **Export all pkarr names…** action, or run:


```sh
iroh-share names export pkarr-names.zip
```


The ZIP is saved on the client computer. Files are named by the z-base-32 public
key: `<key>.key` is the raw 32-byte Ed25519 signing seed, the same format as the
Iroh Link Gateway's `--key-file`, and `<key>.pkarr` is the current signed packet, if one
exists. Aliases are not included. `.pkarr` files use pkarr's
`SignedPacket::as_bytes` format, which is also how Iroh Link Gateway stores signed
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
