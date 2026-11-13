//! Typed control protocol shared by iroh-share clients and the daemon.
#[cfg(feature = "client")]
pub mod client;

use std::{collections::BTreeSet, path::PathBuf};

use irpc::{
    channel::{mpsc, oneshot},
    rpc_requests,
};
use n0_schema::{schema, serialize_service};
use serde::{Deserialize, Serialize};

pub use iroh::EndpointId;
pub use iroh_blobs::{ticket::BlobTicket, Hash};
pub use url::Url;
/// Control shares the blob endpoint; only allowlisted endpoint IDs may use it.
pub const CONTROL_ALPN: &[u8] = b"/iroh-share/control/6";

pub mod v5;

/// Returns true if the daemon rejected a request because it does not know it.
///
/// A daemon from 0.1.13 on rejects such a request on its own and keeps the
/// connection. An older daemon closes the whole connection instead, which
/// looks like any other closed connection, so this returns false for it.
pub fn is_unsupported_request(err: &irpc::Error) -> bool {
    err.remote_code() == Some(irpc::rpc::ErrorCode::RequestDecodeFailed)
}
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowControl {
    pub endpoint: EndpointId,
}
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeControl {
    pub endpoint: EndpointId,
}
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListControl {}

mod download;
pub use download::*;

mod pairing;
pub use pairing::*;

mod names;
pub use names::*;

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub path: PathBuf,
    /// Put directory contents under its basename rather than at the collection root.
    pub include_directory_name: bool,
    /// False keeps the data off Mainline from the start; see [`SetAnnounce`].
    pub announce: bool,
}

/// Fetch into the daemon's public import directory and seed; retain names when updating.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    pub source: DownloadSource,
    /// Update existing data while retaining its ID and linked names; None creates data.
    pub id: Option<u64>,
    /// False drops the collection's single top-level directory, which yields a
    /// new collection hash. None keeps the collection's paths for new data, and
    /// the remembered layout for updates.
    pub include_directory_name: Option<bool>,
    /// False keeps the data off Mainline from the start. None announces new
    /// data and keeps the setting of updated data.
    pub announce: Option<bool>,
}

/// Turn Mainline announcement of one data item's content hash on or off.
///
/// Off keeps seeding: direct tickets still work, but blake3.net links and
/// provider discovery can no longer find this daemon for that content.
/// The choice persists across updates and restarts.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetAnnounce {
    pub id: u64,
    pub announce: bool,
}

/// Per-data Mainline announcement settings, sent in [`WatchEvent::AnnounceUpdated`].
#[schema(Nominal)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnounceSnapshot {
    /// False when the daemon runs with `--no-announce`; nothing is announced.
    pub enabled: bool,
    /// Data IDs whose announcement is turned off; all others are announced while seeding.
    pub disabled: BTreeSet<u64>,
}

/// Report the default public destination on the daemon's filesystem.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetImportDirectory {}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refresh {
    pub id: u64,
}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub source: DownloadSource,
    pub target: PathBuf,
    /// False keeps the data off Mainline from the start; see [`SetAnnounce`].
    pub announce: bool,
}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remove {
    pub id: u64,
}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {}

/// Gracefully stop the daemon; restricted to authorized control clients.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shutdown {}

/// Complete a path using the daemon's filesystem, home and working directory.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePath {
    pub path: PathBuf,
}

#[schema(Nominal)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathKind {
    File,
    Directory,
}

#[schema(Nominal)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathCandidate {
    /// Absolute daemon path; directories include the daemon's trailing separator.
    pub path: PathBuf,
    pub kind: PathKind,
}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathCompletions {
    /// Safe common prefix, computed with the daemon's path syntax.
    pub common_prefix: PathBuf,
    pub candidates: Vec<PathCandidate>,
    /// At most 256 sorted candidates are returned; refine the prefix if truncated.
    pub truncated: bool,
}

/// Counters for importing a known set of source files.
#[schema(Nominal)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
}

/// Download counters include collection metadata. The total may not yet be known.
#[schema(Nominal)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

/// Export counters cover payload files only.
#[schema(Nominal)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    pub state: JobState,
}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobKind {
    Share {
        path: PathBuf,
        include_directory_name: bool,
    },
    Download {
        source: DownloadSource,
        target: PathBuf,
    },
}

#[schema(Nominal)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobError {
    pub message: String,
}

/// Each state carries exactly the information available in that state.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobState {
    Queued,
    Importing {
        progress: ImportProgress,
    },
    Downloading {
        source: DownloadSource,
        progress: DownloadProgress,
    },
    Exporting {
        #[schema(atom = "Hash")]
        root_hash: Hash,
        progress: ExportProgress,
    },
    /// The root hash is available through `ticket.hash()`.
    Seeding {
        #[schema(atom = "BlobTicket")]
        ticket: BlobTicket,
        /// Active requests addressed to this collection root (not distinct peers).
        active_uploads: u32,
    },
    Failed {
        error: JobError,
    },
}

#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WatchEvent {
    /// A complete job snapshot, both during initial enumeration and live updates.
    JobUpdated(Box<Job>),
    /// All initial jobs have been sent, including when there are no jobs.
    SnapshotComplete,
    NameUpdated(Box<Name>),
    NameRemoved {
        label: String,
    },
    JobRemoved {
        id: u64,
    },
    /// Announcement settings: sent before [`Self::SnapshotComplete`], then on every change.
    AnnounceUpdated(AnnounceSnapshot),
}

/// Export all naming keys and current records as a ZIP archive.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportNames {}

/// Export one name's key and current record as a ZIP archive, in the same
/// layout as [`ExportNames`].
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportName {
    pub label: String,
}

/// Restore names from an archive produced by [`ExportNames`] or [`ExportName`].
///
/// The whole archive is validated before any name is added. Keys the daemon
/// already manages are skipped, never overwritten.
#[schema(Nominal)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportNames {
    pub archive: Vec<u8>,
}

pub type RpcResult<T> = Result<T, String>;

#[rpc_requests(message = ControlMessage)]
#[serialize_service(ControlProtocol)]
#[derive(Debug)]
pub enum ControlProtocol {
    #[rpc(tx=oneshot::Sender<RpcResult<Vec<EndpointId>>>)]
    ListControl(ListControl),
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    AllowControl(AllowControl),
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    RevokeControl(RevokeControl),
    #[rpc(tx=oneshot::Sender<RpcResult<Job>>)]
    Share(Share),
    #[rpc(tx=oneshot::Sender<RpcResult<Name>>)]
    CreateName(CreateName),
    #[rpc(tx=oneshot::Sender<RpcResult<Name>>)]
    UpdateName(UpdateName),
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    RemoveName(RemoveName),
    #[rpc(tx=oneshot::Sender<RpcResult<Vec<Name>>>)]
    ListNames(ListNames),
    #[rpc(tx=oneshot::Sender<RpcResult<Job>>)]
    Download(Download),
    #[rpc(tx=oneshot::Sender<RpcResult<Vec<Job>>>)]
    List(List),
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    Remove(Remove),
    #[rpc(tx=mpsc::Sender<RpcResult<WatchEvent>>)]
    Watch(Watch),
    #[rpc(tx=oneshot::Sender<RpcResult<PathCompletions>>)]
    CompletePath(CompletePath),
    #[rpc(tx=oneshot::Sender<RpcResult<PairingTicket>>)]
    CreatePairingTicket(CreatePairingTicket),
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    Shutdown(Shutdown),
    /// Rescan a local directory followed by at least one managed name.
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    Refresh(Refresh),
    #[rpc(tx=oneshot::Sender<RpcResult<Job>>)]
    Import(Import),
    #[rpc(tx=oneshot::Sender<RpcResult<PathBuf>>)]
    GetImportDirectory(GetImportDirectory),
    #[rpc(tx=oneshot::Sender<RpcResult<Vec<u8>>>)]
    ExportNames(ExportNames),
    #[rpc(tx=oneshot::Sender<RpcResult<Vec<u8>>>)]
    ExportName(ExportName),
    #[rpc(tx=oneshot::Sender<RpcResult<Vec<ImportedName>>>)]
    ImportNames(ImportNames),
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    SetAnnounce(SetAnnounce),
}
