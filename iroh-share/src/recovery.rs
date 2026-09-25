use iroh_share_proto::{BlobTicket, DownloadSource, JobKind};
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
        #[serde(default = "included_directory_name")]
        include_directory_name: bool,
    },
    Download {
        #[serde(alias = "ticket", deserialize_with = "deserialize_source")]
        source: DownloadSource,
        target: PathBuf,
        phase: DownloadPhase,
    },
}
impl SavedData {
    pub fn new(kind: &JobKind) -> Self {
        match kind {
            JobKind::Share {
                path,
                include_directory_name,
            } => Self::Share {
                path: path.clone(),
                include_directory_name: *include_directory_name,
            },
            JobKind::Download { source, target } => Self::Download {
                source: source.clone(),
                target: target.clone(),
                phase: DownloadPhase::Downloading,
            },
        }
    }
    pub fn kind(&self) -> JobKind {
        match self {
            Self::Share {
                path,
                include_directory_name,
            } => JobKind::Share {
                path: path.clone(),
                include_directory_name: *include_directory_name,
            },
            Self::Download { source, target, .. } => JobKind::Download {
                source: source.clone(),
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

// Saved ticket downloads and typed hash downloads share the same recovery phases.
fn deserialize_source<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<DownloadSource, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Source(DownloadSource),
        Ticket(BlobTicket),
    }
    Ok(match Input::deserialize(deserializer)? {
        Input::Source(source) => source,
        Input::Ticket(ticket) => ticket.try_into().map_err(serde::de::Error::custom)?,
    })
}

fn included_directory_name() -> bool {
    true
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    #[test]
    fn share_layout_survives_storage_and_missing_option_keeps_directory_name() {
        let saved: SavedData = serde_json::from_str(r#"{"Share":{"path":"/data/bar"}}"#).unwrap();
        assert!(matches!(
            saved.kind(),
            JobKind::Share {
                include_directory_name: true,
                ..
            }
        ));
        for include_directory_name in [false, true] {
            let saved = SavedData::new(&JobKind::Share {
                path: "/data/bar".into(),
                include_directory_name,
            });
            let restored: SavedData =
                serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
            assert!(
                matches!(restored.kind(), JobKind::Share { include_directory_name: value, .. } if value == include_directory_name)
            );
        }
    }
}
