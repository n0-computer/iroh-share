//! The control protocol of daemons and clients up to 0.1.13, on [`CONTROL_ALPN`].
//!
//! The daemon still serves it for older clients. Requests are identified by
//! their position in [`ControlProtocol`], so its variants must never be
//! reordered. Types that did not change are shared with the current protocol.
use std::path::PathBuf;

use irpc::{
    channel::{mpsc, oneshot},
    rpc_requests,
};
use serde::{Deserialize, Serialize};

use crate::{
    AllowControl, AnnounceSnapshot, CompletePath, CreateName, CreatePairingTicket, DownloadSource,
    EndpointId, ExportName, ExportNames, GetImportDirectory, ImportNames, ImportedName, Job, List,
    ListControl, ListNames, Name, PairingTicket, PathCompletions, Refresh, Remove, RemoveName,
    RevokeControl, RpcResult, SetAnnounce, Shutdown, UpdateName, Watch,
};

#[path = "gateway.rs"]
mod gateway;
pub use gateway::*;

pub const CONTROL_ALPN: &[u8] = b"/iroh-share/control/5";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub path: PathBuf,
    pub include_directory_name: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub source: DownloadSource,
    pub target: PathBuf,
}

/// Import keeping the collection's paths (new data) or the remembered layout (updates).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    pub source: DownloadSource,
    pub id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportWithOptions {
    pub source: DownloadSource,
    pub id: Option<u64>,
    pub include_directory_name: bool,
    pub announce: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchAnnounce {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WatchEvent {
    JobUpdated(Box<Job>),
    SnapshotComplete,
    NameUpdated(Box<Name>),
    NameRemoved { label: String },
    JobRemoved { id: u64 },
    GatewayUpdated(GatewaySnapshot),
}

#[rpc_requests(message = ControlMessage)]
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
    #[rpc(tx=oneshot::Sender<RpcResult<Job>>)]
    ImportWithOptions(ImportWithOptions),
    #[rpc(tx=oneshot::Sender<RpcResult<()>>)]
    SetAnnounce(SetAnnounce),
    #[rpc(tx=mpsc::Sender<RpcResult<AnnounceSnapshot>>)]
    WatchAnnounce(WatchAnnounce),
}
