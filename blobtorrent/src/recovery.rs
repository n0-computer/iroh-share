use blobtorrent_proto::{BlobTicket, JobKind};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadPhase {
    Downloading,
    Exporting,
    Seeding,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum SavedData {
    Share {
        path: PathBuf,
    },
    Download {
        ticket: BlobTicket,
        target: PathBuf,
        phase: DownloadPhase,
    },
}
impl SavedData {
    pub fn new(kind: &JobKind) -> Self {
        match kind {
            JobKind::Share { path } => Self::Share { path: path.clone() },
            JobKind::Download { ticket, target } => Self::Download {
                ticket: ticket.clone(),
                target: target.clone(),
                phase: DownloadPhase::Downloading,
            },
        }
    }
    pub fn kind(&self) -> JobKind {
        match self {
            Self::Share { path } => JobKind::Share { path: path.clone() },
            Self::Download { ticket, target, .. } => JobKind::Download {
                ticket: ticket.clone(),
                target: target.clone(),
            },
        }
    }
    pub fn phase(&self) -> DownloadPhase {
        match self {
            Self::Download { phase, .. } => *phase,
            _ => DownloadPhase::Downloading,
        }
    }
}
pub struct Checkpoint {
    pub id: u64,
    pub phase: DownloadPhase,
    pub ack: oneshot::Sender<Result<(), String>>,
}
pub async fn checkpoint(
    tx: Option<&mpsc::Sender<Checkpoint>>,
    id: u64,
    phase: DownloadPhase,
) -> anyhow::Result<()> {
    if let Some(tx) = tx {
        let (ack, rx) = oneshot::channel();
        tx.send(Checkpoint { id, phase, ack })
            .await
            .map_err(|_| anyhow::anyhow!("controller stopped"))?;
        rx.await?.map_err(anyhow::Error::msg)?;
    }
    Ok(())
}
