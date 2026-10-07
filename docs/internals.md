# Internals

How the daemon and its control protocol work. This is for people writing frontends or working on the code; [UI.md](../iroh-share-proto/UI.md) is the capability guide for frontend authors.

A background service for sharing and downloading iroh blob collections. The daemon
owns one iroh endpoint and one filesystem blob store shared by all transfers. Its
authenticated QUIC iRPC control API supports starting, listing, watching, and
removing jobs.

The Cargo workspace has five crates:

- `iroh-share-proto`: typed iRPC messages and state snapshots, with optional
  `client` helpers for native clients.
- `iroh-share`: daemon and command-line client in one binary.
- `iroh-share-tui`: Ratatui terminal interface using the same control protocol.
- `iroh-share-gui`: egui desktop interface using the same control protocol.
- `iroh-share-web`: browser interface, Rust compiled to WebAssembly, using the
  same control protocol over a relay-only iroh endpoint.

Normally, a shared Mainline publisher announces completed collections and retries
on discovery failures. Direct ticket transfers work independently of Mainline.
The announcement library is pinned to a Git revision in `Cargo.toml`.

## Control protocol

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
existing job, `NameUpdated` for each name, and one `GatewayUpdated` for older clients, then `SnapshotComplete` (also for an empty daemon). Subsequent events
are complete `JobUpdated`/`NameUpdated` snapshots or `JobRemoved { id }`/`NameRemoved { label }`. Clients replace their
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

## Storage and seeding

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

## Persistent data

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

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

GitHub Actions runs formatting, Clippy, and workspace tests on pull requests and pushes to `main`. The **Binary releases** workflow builds the daemon, CLI, TUI and GUI for Windows x64 and macOS Apple silicon, static daemon, CLI and TUI binaries for x64 and arm64 Linux, the Windows and macOS installers, and Synology packages. It can be run manually to produce workflow artifacts without a release. Pushing a `v*` tag publishes everything as a GitHub release with one `SHA256SUMS` file. The builds are unsigned and not notarized, so OS download protections may require explicit approval to run them.
