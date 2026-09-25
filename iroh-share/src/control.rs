use anyhow::{ensure, Context, Result};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_share_proto::{ControlProtocol, EndpointId};
use iroh_share_proto::{PairingSecret, PairingStatus, PairingTicket};
use std::sync::{Arc, Mutex};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct Access {
    path: PathBuf,
    owner: EndpointId,
    mutation: Arc<Mutex<()>>,
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
            mutation: Arc::new(Mutex::new(())),
            allowed,
        })
    }
    pub fn list(&self) -> Vec<EndpointId> {
        self.allowed.borrow().iter().copied().collect()
    }
    pub fn update(&self, endpoint: EndpointId, allow: bool) -> Result<()> {
        let _guard = self
            .mutation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// Independent single-client invitations, valid for this daemon process.
#[derive(Debug, Clone, Default)]
pub struct Pairing {
    invitations: Arc<Mutex<Vec<Invitation>>>,
}
#[derive(Debug)]
struct Invitation {
    secret: PairingSecret,
    redeemed_by: Option<EndpointId>,
}
impl Pairing {
    pub fn issue(&self, addr: iroh::EndpointAddr) -> PairingTicket {
        let secret = PairingSecret::from_bytes(rand::random());
        self.invitations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Invitation {
                secret: secret.clone(),
                redeemed_by: None,
            });
        PairingTicket { addr, secret }
    }
    fn redeem(&self, access: &Access, peer: EndpointId, secret: &PairingSecret) -> PairingStatus {
        let mut invitations = self
            .invitations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(invitation) = invitations
            .iter_mut()
            .find(|entry| entry.secret.matches(secret))
        else {
            return PairingStatus::Invalid;
        };
        if let Some(owner) = invitation.redeemed_by {
            // Retrying a lost reply must not reauthorize a revoked identity.
            return if owner == peer && access.list().contains(&peer) {
                PairingStatus::Accepted
            } else {
                PairingStatus::Invalid
            };
        }
        if let Err(error) = access.update(peer, true) {
            tracing::warn!(%error, "Could not persist pairing authorization");
            return PairingStatus::StorageFailure;
        }
        invitation.redeemed_by = Some(peer);
        PairingStatus::Accepted
    }
}

#[derive(Debug)]
pub struct PairingProtocol {
    pub access: Access,
    pub pairing: Pairing,
}
impl ProtocolHandler for PairingProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // A completed authenticated handshake binds the invitation to the peer.
        let exchange = async {
            let (mut send, mut recv) = connection.accept_bi().await?;
            let bytes = recv.read_to_end(32).await?;
            let status = if let Ok(secret) = <[u8; 32]>::try_from(bytes) {
                self.pairing.redeem(
                    &self.access,
                    connection.remote_id(),
                    &PairingSecret::from_bytes(secret),
                )
            } else {
                PairingStatus::Invalid
            };
            send.write_all(&[status as u8]).await?;
            send.finish()?;
            // Keep the connection alive until the client has received the reply.
            connection.closed().await;
            Ok::<_, anyhow::Error>(())
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), exchange).await;
        connection.close(0u32.into(), b"pairing complete");
        Ok(())
    }
}

/// Include direct hints for same-machine pairing as well as advertised addresses.
pub fn pairing_address(endpoint: &iroh::Endpoint) -> iroh::EndpointAddr {
    let mut addr = endpoint.addr();
    for socket in endpoint.bound_sockets() {
        let ip = if socket.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        };
        addr = addr.with_ip_addr(std::net::SocketAddr::new(ip, socket.port()));
    }
    addr
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
    use iroh::{endpoint::presets, protocol::Router, Endpoint};
    use iroh_blobs::{store::fs::FsStore, BlobsProtocol};
    use iroh_share_proto::{ControlMessage, List, Watch, WatchEvent, CONTROL_ALPN};
    use std::time::Duration;

    #[test]
    fn invitations_are_independent_durable_single_client_grants() -> Result<()> {
        let root = tempfile::tempdir()?;
        let owner = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let first = iroh::SecretKey::from_bytes(&[2; 32]).public();
        let second = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let access = Access::load(root.path(), owner)?;
        let pairing = Pairing::default();
        let one = pairing.issue(owner.into());
        let two = pairing.issue(owner.into());
        assert_eq!(
            pairing.redeem(&access, first, &PairingSecret::from_bytes([0; 32])),
            PairingStatus::Invalid
        );
        // A failed durable write leaves the invitation available for retry.
        std::fs::create_dir(root.path().join("control-allowed.json"))?;
        assert_eq!(
            pairing.redeem(&access, first, &one.secret),
            PairingStatus::StorageFailure
        );
        assert!(!access.list().contains(&first));
        std::fs::remove_dir(root.path().join("control-allowed.json"))?;
        assert_eq!(
            pairing.redeem(&access, first, &one.secret),
            PairingStatus::Accepted
        );
        assert_eq!(
            pairing.redeem(&access, first, &one.secret),
            PairingStatus::Accepted
        );
        assert_eq!(
            pairing.redeem(&access, second, &one.secret),
            PairingStatus::Invalid
        );
        assert_eq!(
            pairing.redeem(&access, second, &two.secret),
            PairingStatus::Accepted
        );
        assert!(Access::load(root.path(), owner)?.list().contains(&first));
        assert!(Access::load(root.path(), owner)?.list().contains(&second));
        access.update(first, false)?;
        assert_eq!(
            pairing.redeem(&access, first, &one.secret),
            PairingStatus::Invalid
        );
        assert!(!access.list().contains(&first));
        assert_eq!(
            Pairing::default().redeem(&access, second, &two.secret),
            PairingStatus::Invalid
        );
        Ok(())
    }

    #[tokio::test]
    async fn ticket_setup_saves_identity_and_address_and_races_have_one_winner() -> Result<()> {
        use iroh_share_proto::client::{
            configured_address, load_or_create_key, pair, redeem_pairing, ControlClient,
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            let root = tempfile::tempdir()?;
            let config = tempfile::tempdir()?;
            let endpoint = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let a = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let b = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let access = Access::load(root.path(), endpoint.id())?;
            let pairing = Pairing::default();
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            let router = Router::builder(endpoint.clone())
                .accept(
                    CONTROL_ALPN,
                    Control::new(access.clone(), irpc::LocalSender::from(tx)),
                )
                .accept(
                    iroh_share_proto::PAIRING_ALPN,
                    PairingProtocol {
                        access: access.clone(),
                        pairing: pairing.clone(),
                    },
                )
                .spawn();
            let service = tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    if let ControlMessage::List(message) = message {
                        let _ = message.tx.send(Ok(Vec::new())).await;
                    }
                }
            });
            let ticket = pairing.issue(endpoint.addr());
            let other_ticket = pairing.issue(endpoint.addr());
            let mut invalid = ticket.clone();
            invalid.secret = PairingSecret::from_bytes([0; 32]);
            assert!(redeem_pairing(&a, &invalid).await.is_err());
            pair(config.path(), &ticket).await?;
            let id = load_or_create_key(&config.path().join("control-client.key"))?.public();
            assert!(access.list().contains(&id));
            assert_eq!(
                configured_address(config.path())?,
                Some(ticket.addr.clone())
            );
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(config.path().join("client.json"))?)?;
            assert!(saved.get("secret").is_none());
            // The same persistent identity can recover a lost response safely.
            pair(config.path(), &ticket).await?;
            let client = ControlClient::connect_configured(config.path()).await?;
            assert!(client.list().await?.is_empty());
            assert!(redeem_pairing(&a, &ticket).await.is_err());
            let (one, two) = tokio::join!(
                redeem_pairing(&a, &other_ticket),
                redeem_pairing(&b, &other_ticket)
            );
            assert_ne!(one.is_ok(), two.is_ok());
            let winner = if one.is_ok() { a.id() } else { b.id() };
            let loser = if one.is_ok() { b.id() } else { a.id() };
            assert!(access.list().contains(&winner));
            assert!(!access.list().contains(&loser));
            assert!(Access::load(root.path(), endpoint.id())?
                .list()
                .contains(&winner));
            access.update(id, false)?;
            assert!(pair(config.path(), &ticket).await.is_err());
            drop(client);
            a.close().await;
            b.close().await;
            router.shutdown().await?;
            service.abort();
            Ok::<_, anyhow::Error>(())
        })
        .await?
    }

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
