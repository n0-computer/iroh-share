# Building a Blobtorrent UI

This is a capability guide for clients of the irpc control protocol. Frontends can
choose their own layout and supported features; they do not need identical screens
or synchronized feature sets. The Rust types in `src/lib.rs`, `src/names.rs`,
`src/gateway.rs`, and `src/pairing.rs` define the wire contract.

## Connection and identity

A client owns its endpoint secret key and configuration directory. It does not need
access to the daemon's state directory. Use a platform-specific client configuration
directory and persist the key across launches.

A `PairingTicket` contains the daemon's endpoint address and a one-time secret.
Accept it through a paste field or command-line argument, redeem it with
`client::pair`, and then connect using `ControlClient::connect_configured`.
Pairing enrolls the client's authenticated endpoint ID and saves the daemon address.
Do not log or persist the ticket. Display parse, connection, and redemption failures
without discarding the user's ability to retry. Multiple invitations can coexist;
each enrolls one client. Invitations expire when the daemon restarts, while grants
persist. The daemon prints an invitation on first startup; an authorized client can
request another through `CreatePairingTicket` (CLI: `blobtorrent control pair`).

Control and blob traffic use the daemon's shared iroh endpoint, with distinct ALPNs.
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

## Data

Present jobs as **Data** or paths; IDs are internal references and need not be shown.
`JobKind::Share` supplies the source path; `JobKind::Download` supplies the target
path and source ticket. Label both simply **Path**.

- `Share { path }` imports a file or directory and seeds it. Shared directories are
  watched for changes; linked names follow the resulting content updates.
- `Download { ticket, target }` downloads and exports content, then seeds it.
  Parse a typed `BlobTicket`; a content URL alone is not a `Download` argument.
- `Remove { id }` stops/removes the tracked data without deleting user files.
  Explain this distinction in the confirmation. A name that references removed data
  cannot keep following it; names have their own lifecycle.

Render progress according to `JobState`, rather than optional fields scattered
across one generic record:

| State | Information available |
| --- | --- |
| Queued | Kind and requested path |
| Importing | File and byte counts with totals |
| Downloading | Source ticket, bytes done, optional total |
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
IPv4 index server override. No override means Mainline rendezvous discovery.

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
