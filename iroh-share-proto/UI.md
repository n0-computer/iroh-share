# Building an Iroh Share UI

Follow [UX.md](UX.md) for shared publishing workflows and interaction rules.

This is a capability guide for clients of the irpc control protocol. Frontends can
choose their own layout and supported features; they do not need identical screens
or synchronized feature sets. The Rust types in `src/lib.rs`, `src/names.rs`,
`src/gateway.rs`, `src/download.rs`, and `src/pairing.rs` define the wire contract.

## Connection and identity

A client owns its endpoint secret key and configuration directory. It does not need
access to the daemon's state directory. Use a platform-specific client configuration
directory and persist the key across launches.

A `PairingTicket` contains the daemon's endpoint address and a one-time secret.
Accept it through a paste field or command-line argument, redeem it with
`client::pair`, and then connect using `ControlClient::connect_configured`.
Pairing enrolls the client's authenticated endpoint ID and saves the daemon address.

A client can be paired with several daemons using one identity. `client::pair` adds
the daemon to the saved list and makes it current. `saved_daemons`, `select_daemon`,
`rename_daemon`, and `forget_daemon` manage the list; `connect_configured` always
connects to the current daemon, which is the one used most recently. When switching,
discard the previous daemon's state, queued commands, and per-daemon preferences
such as same-filesystem access, as after a reconnect. Forgetting a daemon is local:
the daemon keeps the client authorized until it is revoked.
Do not log or persist the ticket. Display parse, connection, and redemption failures
without discarding the user's ability to retry. Multiple invitations can coexist;
each enrolls one client. Invitations expire when the daemon restarts, while grants
persist. The daemon prints an invitation on first startup; an authorized client can
request another through `CreatePairingTicket` (CLI: `iroh-share control pair`).

Control and blob traffic use the daemon's shared iroh endpoint, with distinct ALPNs.
The control ALPN is `/iroh-share/control/5`; clients and daemon must agree on it.
Control access requires an allowlisted endpoint ID. Administrative clients can use
`ListControl`, `AllowControl`, and `RevokeControl`. Revocation also closes active
control connections. These capabilities are optional UI features, not prerequisites
for pairing or ordinary data management.

## Live state and reconnection

Subscribe with `Watch`. Each `JobUpdated`, `NameUpdated`, or `GatewayUpdated` event
contains a complete snapshot of that entity. Replace the cached entity; do not
interpret an update as a partial patch. Jobs are keyed by `id`, names by `label`.
Apply `JobRemoved` and `NameRemoved` by removing the corresponding entity.

`SnapshotComplete` marks the end of initial enumeration, including an empty data
set. Keep mutations disabled until it arrives. The initial stream includes names
and gateway state as well as data. `List`, `ListNames`, and `GetGateway` provide
unary snapshots when continuous observation is unnecessary.

On disconnect, disable actions and clear or visibly mark stale state. Reconnect,
rebuild the snapshot, and discard queued mutations, selections, completion results,
and confirmations that could refer to a different daemon or reused IDs. Do not
replay a destructive action automatically. Bound connection and initial snapshot
waits, show the error, and allow retries. Keep network work off the UI thread.

## Publishing workflow

Make publishing your own content the primary Data workflow. On a local setup,
accept file/folder drops or native selection and offer a name during publication.
On a remote setup, accept a sendme collection ticket through a paste field or a
`.ticket`/`.sendme` text file drop. Present a review before starting a ticket import.
Native desktop backends may expose file drops without exposing dragged text;
pasting remains available. Ticket-file reads use the client's filesystem and do
not require the same-filesystem setting; publishing a dropped folder does.

`Import { source, id: None }` fetches, exports and seeds content in a fresh folder
under the daemon's public import directory. `GetImportDirectory` reports that
path. The default is the platform Downloads directory's `Iroh Share` subfolder
(or `~/Iroh Share` when Downloads is unavailable); `daemon --import-dir <path>`
overrides it. This directory is separate from the private state/blob store.
Show the actual exported path, not a client-side guess at the daemon's filesystem.

`Import { source, id: Some(id) }` updates idle or failed data under the same ID,
preserving linked names and their keys. It uses a fresh destination and retains
previous exported files. The preceding seeded collection is pinned during update.
Import progress and failures arrive through Watch. The accepted request is durable;
a failed transfer can be retried with another ticket on the same row. Keep the
sender running until import completes. Both import variants use the shared typed
`DownloadSource` parser; the GUI's publishing dialog accepts collection tickets.

`Refresh { id }` explicitly rescans a shared local directory followed by at least
one `NameTarget::Job` name. Files, downloads, unnamed directories and busy imports
are rejected. Refresh keeps the same data ID and names; linked names follow the
resulting hash. Automatic directory monitoring also remains active. A client with
local filesystem access can show Refresh only on eligible directory rows; remote
publishing uses Update from ticket instead.

Manage all content names in Data: job targets, fixed blake3.net URLs (including
subpaths), and names whose linked data is unavailable. Show their public pkarr URL
and publication state. Explain the difference between following data and pointing
to a fixed URL. Ordinary URL names and downloads are secondary collapsed sections.
Provide direct Copy ticket and copy/open public-link actions on seeded data.
Frontends do not need matching layouts; the TUI can use paste and path completion
without native drag-and-drop.

## Data

Present jobs as **Data** or paths; IDs are internal references and need not be shown.
`JobKind::Share` supplies the source path; `JobKind::Download` supplies the target
path and typed source. Label both simply **Path**.

- `Share { path, include_directory_name }` imports a file or directory and seeds it. Shared directories are
  watched for changes; linked names follow the resulting content updates.
- `Download { source, target }` downloads and exports a collection, then seeds it.
  Parse a `DownloadSource` from a `blake3.net` root URL, a bare z32/hex hash, or a
  sendme-compatible collection ticket. Send one RPC with the BLAKE3 `hash`,
  `providers: Vec<EndpointAddr>` (empty if unknown), and `DiscoveryMode`.
  `Disabled` restricts the daemon to the supplied addresses and requires at
  least one provider. `Mainline` allows discovery if those providers cannot
  complete the download. URL/hash parsing selects `Mainline`; ticket parsing
  preserves its relay/direct address hints and selects `Disabled`. Clients can
  offer a control to enable discovery for tickets; they never perform discovery
  themselves. URLs with subpaths, queries, or fragments are rejected, since the
  operation downloads a whole collection.
- `Remove { id }` stops/removes the tracked data without deleting user files.
  Explain this distinction in the confirmation. A name that references removed data
  cannot keep following it; names have their own lifecycle.

Render progress according to `JobState`, rather than optional fields scattered
across one generic record:

| State | Information available |
| --- | --- |
| Queued | Kind and requested path |
| Importing | File and byte counts with totals |
| Downloading | Hash, provider addresses, discovery policy, bytes done, optional total |
| Exporting | Root hash, payload file and byte counts |
| Seeding | Required ticket, with the root hash obtained from `ticket.hash()` |
| Failed | Error message |

An unknown download total is indeterminate, not zero. State updates can move back
to importing when shared directory contents change. All data states are persisted
by the daemon. Import work restarts after daemon restart; partial downloads reuse
local data; exporting resumes export; completed seeding data does not re-export.
A UI must not recreate operations just because it reconnects.

For seeding data, show `https://<z-base-32 hash>.blake3.net/` prominently, with
open and copy actions. Use the raw 32 hash bytes for canonical z-base-32 encoding.
Offer the full blob ticket as a secondary copy action for downloading in clients.
Opening an exported directory is a local desktop capability, subject to the
filesystem rules below.

## Paths and local desktop integration

`CompletePath { path }` operates on the **daemon's** filesystem, including its home
directory, working directory, and path syntax. `PathCompletions` includes a safe
common prefix, typed file/directory candidates, and a truncation flag. At most 256
sorted candidates are returned; ask for a narrower prefix when truncated. Retain
the returned separator convention. Associate requests with the current input and
discard responses after edits, reconnects, or superseding requests.

Native folder pickers, dropped folders, and opening paths in Finder/Explorer use
the **client's** filesystem. Enable these only when the user establishes that the
daemon shares that filesystem. A loopback connection is a hint, not proof (VMs,
containers, and namespaces can differ). Remote clients should use daemon-side
path completion. A browser drop supplies browser file handles/content, not a
reliable absolute path on the daemon.

Desktop clients may share a dropped file or directory directly once the same
filesystem assumption is established. Do not silently queue drops while offline.
Clipboard and browser actions are client-local and require no daemon RPC.

## Pkarr names

`CreateName { label, target }` creates a signing identity managed by the daemon.
`UpdateName { label, target }` changes its target while retaining that identity.
`RemoveName { label }` deletes the managed name and signing key; warn that cached
published records can remain resolvable. `ListNames` and watch updates expose the
public state, never private signing keys.

`NameTarget` is either a parsed `Url` or an existing data ID (`Job`). Use a path
selector for data targets instead of asking users to type an ID. A linked shared
directory follows new content automatically; direct URL targets remain explicit.
Labels and public keys are distinct: display a friendly label with the public
`NameKey::url()` (`https://<key>.pkarr.net/`). Copy/open actions should use that
public name URL, even when the target is a different URL.

Display `Disabled`, `WaitingForJob`, `Publishing`, `Published`, and `Failed`
according to `NameState`. Publication is asynchronous; a successful create/update
response does not imply publication has completed. The daemon persists identities
and targets and republishes records independently of any UI connection.

## Gateway settings

`GetGateway` returns a `GatewaySnapshot`; `SetGateway { config }` persists desired
configuration and restarts or stops the gateway. Watch provides runtime updates.
`GatewayConfig` contains `enabled`, a loopback HTTP listen address, and an optional
IPv4 index server override. No override means the index servers listed by n0.

Without saved settings, the gateway starts enabled on `127.0.0.1:45475`. An explicitly
saved disabled setting is preserved.

The gateway is embedded in the daemon process, uses its own iroh endpoint with zero listening ALPNs and its own
resolver, and browses remote content without depending on the daemon's blob store.
It stays running after the UI exits and starts with the daemon when enabled.
Failures are reported and retried every 30 seconds. An explicit save can retry
immediately. Only loopback HTTP listeners are accepted; the address refers to the
daemon's machine, not necessarily the UI's machine.

Distinguish desired configuration from observed `GatewayState`: `Disabled`,
`Starting`, `Running { listen, endpoint }`, or `Failed { error }`. The actual listen
address matters when configured with port zero. Saving successfully does not mean
the listener is already running. Preserve unsaved edits while watch updates arrive;
show runtime status independently. Configure the browser extension to use the
reported HTTP address. The gateway does not provide a root homepage.

## Interaction principles

Keep mutation failures visible and preserve editable input for correction. Confirm
removal without blocking unrelated live updates. Prevent repeated submissions while
a request is pending. Never display a disconnected cache as live state. Persist UI
preferences separately from daemon-owned data and configuration. A frontend may
omit capabilities it cannot support well; document its own limits instead of
requiring every frontend to match it.

Seeding snapshots include `active_uploads`, the number of active requests for the
collection root. Clients can show this beside Seeding. Counts reset on restart
and drop when requests complete, fail, or disconnect. Individual child-blob
requests are not attributed to collections; counts describe requests, not people.

## Local installation lifecycle

`Shutdown` asks the daemon to stop gracefully; it uses the same endpoint allowlist
as other control requests. The response acknowledges the request, not completion
of shutdown. Installer helpers wait for the daemon's state-directory lock to be
released before replacing binaries or reporting that the daemon is stopped.

Local installer setup can authenticate as the local owner using
`ControlClient::connect_local`, mint an invitation, and redeem it for a new GUI
identity. This is an explicit same-user setup operation. Normal GUI connections
continue using only their own configuration and key. Installer setup must preserve
an existing configured daemon and must never silently replace a remote connection.

## Name backup export

`ExportNames {}` returns `RpcResult<Vec<u8>>` containing the complete ZIP archive.
This authenticated operation exports every naming key,
including names attached to content and keys without a record. Save the archive
on the client computer; never include its bytes in logs or the Watch stream.
Entries are `<public-key>.key`, the raw 32-byte signing seed, and optional
`<public-key>.pkarr`, which is public key + signature + big-endian 64-bit timestamp
+ DNS packet, the pkarr `SignedPacket::as_bytes` format.

`ExportRecord { label }` returns one name's current record in the same signed
packet layout, without the private key. It fails for unknown names and for names
that have not produced a record yet.

`NameTarget::Records(String)` contains zone-style DNS records, one per line:
`owner TTL IN TYPE value`. Owners are relative to the public key; @ is its root.
The daemon validates records before saving and enforces the 1000-byte signed
DNS payload limit. Zone-file directives and external owner names are rejected.
These targets use PublishingRecords / PublishedRecords states without a URL.
They persist, republish, and export like URL and data-bound names.
