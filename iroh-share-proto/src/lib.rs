//! Typed control protocol shared by iroh-share clients and the daemon.
#[cfg(feature = "client")]
pub mod client;

use std::path::PathBuf;

use irpc::{
    channel::{mpsc, oneshot},
    rpc_requests,
};
use serde::{Deserialize, Serialize};

pub use iroh::EndpointId;
pub use iroh_blobs::{ticket::BlobTicket, Hash};
pub use url::Url;
/// Control shares the blob endpoint; only allowlisted endpoint IDs may use it.
pub const CONTROL_ALPN: &[u8] = b"/iroh-share/control/5";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowControl {
    pub endpoint: EndpointId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeControl {
    pub endpoint: EndpointId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListControl {}

mod download;
pub use download::*;

mod gateway;
pub use gateway::*;

mod pairing;
pub use pairing::*;

mod names;
pub use names::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub path: PathBuf,
    /// Put directory contents under its basename rather than at the collection root.
    pub include_directory_name: bool,
}

/// Fetch into the daemon's public import directory and seed; retain names when updating.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    pub source: DownloadSource,
    /// Update existing data while retaining its ID and linked names; None creates data.
    pub id: Option<u64>,
}

/// Report the default public destination on the daemon's filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetImportDirectory {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refresh {
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub source: DownloadSource,
    pub target: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remove {
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {}

/// Gracefully stop the daemon; restricted to authorized control clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shutdown {}

/// Complete a path using the daemon's filesystem, home and working directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePath {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathCandidate {
    /// Absolute daemon path; directories include the daemon's trailing separator.
    pub path: PathBuf,
    pub kind: PathKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathCompletions {
    /// Safe common prefix, computed with the daemon's path syntax.
    pub common_prefix: PathBuf,
    pub candidates: Vec<PathCandidate>,
    /// At most 256 sorted candidates are returned; refine the prefix if truncated.
    pub truncated: bool,
}

/// Counters for importing a known set of source files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
}

/// Download counters include collection metadata. The total may not yet be known.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

/// Export counters cover payload files only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    pub state: JobState,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobError {
    pub message: String,
}

/// Each state carries exactly the information available in that state.
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
        root_hash: Hash,
        progress: ExportProgress,
    },
    /// The root hash is available through `ticket.hash()`.
    Seeding {
        ticket: BlobTicket,
        /// Active requests addressed to this collection root (not distinct peers).
        active_uploads: u32,
    },
    Failed {
        error: JobError,
    },
}

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
    GatewayUpdated(GatewaySnapshot),
}

/// Export all naming keys and current records as a ZIP archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportNames {}

/// Export one name's current record as a Pkarr signed packet:
/// public key (32 bytes), signature (64), sequence (8, big endian), DNS packet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecord {
    pub label: String,
}

pub type RpcResult<T> = Result<T, String>;

#[rpc_requests(message = ControlMessage, no_spans)]
#[derive(Debug, Serialize, Deserialize)]
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
    #[rpc(tx=oneshot::Sender<RpcResult<GatewaySnapshot>>)]
    GetGateway(GetGateway),
    #[rpc(tx=oneshot::Sender<RpcResult<GatewaySnapshot>>)]
    SetGateway(SetGateway),
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
    ExportRecord(ExportRecord),
}
