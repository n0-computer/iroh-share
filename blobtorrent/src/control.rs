use anyhow::{ensure, Context, Result};
use blobtorrent_proto::{ControlProtocol, EndpointId};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct Access {
    path: PathBuf,
    owner: EndpointId,
    allowed: watch::Sender<BTreeSet<EndpointId>>,
}
impl Access {
    pub fn load(root: &Path, owner: EndpointId) -> Result<Self> {
        let path = root.join("control-allowed.json");
        let mut allowed: BTreeSet<EndpointId> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("invalid control allowlist")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(error) => return Err(error.into()),
        };
        allowed.insert(owner);
        let (allowed, _) = watch::channel(allowed);
        Ok(Self {
            path,
            owner,
            allowed,
        })
    }
    pub fn list(&self) -> Vec<EndpointId> {
        self.allowed.borrow().iter().copied().collect()
    }
    pub fn update(&self, endpoint: EndpointId, allow: bool) -> Result<()> {
        ensure!(
            allow || endpoint != self.owner,
            "cannot revoke the local owner identity"
        );
        let mut allowed = self.allowed.borrow().clone();
        if allow {
            allowed.insert(endpoint);
        } else {
            allowed.remove(&endpoint);
        }
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", rand::random::<u64>()));
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&allowed)?)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        self.allowed.send_replace(allowed);
        Ok(())
    }
}

#[derive(Debug)]
pub struct Control {
    access: Access,
    protocol: irpc_iroh::IrohProtocol<ControlProtocol>,
}
impl Control {
    pub fn new(access: Access, sender: irpc::LocalSender<ControlProtocol>) -> Self {
        Self {
            access,
            protocol: irpc_iroh::IrohProtocol::with_sender(sender),
        }
    }
}
impl ProtocolHandler for Control {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // This runs only after the authenticated handshake, never in 0-RTT.
        let peer = connection.remote_id();
        let mut allowed = self.access.allowed.subscribe();
        if !allowed.borrow_and_update().contains(&peer) {
            tracing::warn!(%peer, "Rejected control connection: endpoint is not allowed");
            connection.close(1u32.into(), b"control endpoint not authorized");
            return Ok(());
        }
        let serve = self.protocol.accept(connection.clone());
        tokio::pin!(serve);
        loop {
            tokio::select! {
                result = &mut serve => return result,
                changed = allowed.changed() => {
                    if changed.is_err() || !allowed.borrow_and_update().contains(&peer) {
                        connection.close(1u32.into(), b"control access revoked");
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blobtorrent_proto::{ControlMessage, List, Watch, WatchEvent, CONTROL_ALPN};
    use iroh::{endpoint::presets, protocol::Router, Endpoint};
    use iroh_blobs::{store::fs::FsStore, BlobsProtocol};
    use std::time::Duration;

    #[tokio::test]
    async fn whitelist_gates_control_only_and_revokes_live_connections() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(15), async {
            let root = tempfile::tempdir()?;
            let store = FsStore::load(root.path().join("blobs")).await?;
            let endpoint = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let peer = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let owner = iroh::SecretKey::from_bytes(&[42; 32]).public();
            let access = Access::load(root.path(), owner)?;
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            let router = Router::builder(endpoint.clone())
                .accept(iroh_blobs::ALPN, BlobsProtocol::new(&store, None))
                .accept(
                    CONTROL_ALPN,
                    Control::new(access.clone(), irpc::LocalSender::from(tx)),
                )
                .spawn();
            let service = tokio::spawn(async move {
                let mut watchers = Vec::new();
                while let Some(message) = rx.recv().await {
                    match message {
                        ControlMessage::List(message) => {
                            let _ = message.tx.send(Ok(Vec::new())).await;
                        }
                        ControlMessage::Watch(message) => {
                            let _ = message.tx.send(Ok(WatchEvent::SnapshotComplete)).await;
                            watchers.push(message.tx);
                        }
                        _ => panic!("unexpected request"),
                    }
                }
            });
            let client =
                irpc_iroh::client::<ControlProtocol>(peer.clone(), endpoint.addr(), CONTROL_ALPN);
            assert!(client.rpc(List {}).await.is_err());
            let public = peer.connect(endpoint.addr(), iroh_blobs::ALPN).await?;
            assert!(public.close_reason().is_none());
            access.update(peer.id(), true)?;
            assert!(Access::load(root.path(), owner)?
                .list()
                .contains(&peer.id()));
            let client =
                irpc_iroh::client::<ControlProtocol>(peer.clone(), endpoint.addr(), CONTROL_ALPN);
            assert!(client.rpc(List {}).await?.is_ok());
            let mut events = client.server_streaming(Watch {}, 8).await?;
            assert!(matches!(
                events.recv().await?,
                Some(Ok(WatchEvent::SnapshotComplete))
            ));
            access.update(peer.id(), false)?;
            assert!(matches!(events.recv().await, Err(_) | Ok(None)));
            assert!(client.rpc(List {}).await.is_err());
            assert!(!Access::load(root.path(), owner)?
                .list()
                .contains(&peer.id()));
            assert!(access.update(owner, false).is_err());
            assert!(public.close_reason().is_none());
            peer.close().await;
            router.shutdown().await?;
            service.abort();
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
}
