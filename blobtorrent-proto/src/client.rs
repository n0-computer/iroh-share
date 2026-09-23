//! Optional local client support, for native control clients.
use crate::{
    BlobTicket, ControlProtocol, Download, Job, List, Remove, RpcResult, Share, Watch, WatchEvent,
};
use anyhow::{Context, Result};
use iroh::{endpoint::presets, Endpoint, EndpointAddr, SecretKey};
use irpc::{channel::mpsc, Client};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub fn default_state_dir() -> Result<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|base| base.join("blobtorrent"))
        .context("cannot determine the user state directory; provide --state-dir")
}

#[derive(Clone)]
pub struct ControlClient {
    client: Client<ControlProtocol>,
    endpoint: Endpoint,
    remote: bool,
}

impl ControlClient {
    /// Use a caller-owned iroh endpoint, including an embedded UI's identity.
    pub fn from_endpoint(endpoint: Endpoint, addr: EndpointAddr) -> Self {
        Self {
            client: irpc_iroh::client(endpoint.clone(), addr, crate::CONTROL_ALPN),
            endpoint,
            remote: true,
        }
    }
    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }
    /// Connect using only the client's own configuration and identity.
    /// Never falls back to daemon state files.
    pub async fn connect_configured(config_dir: &Path) -> Result<Self> {
        let server = configured_address(config_dir)?
            .context("no daemon configured; pair this client using a daemon invitation")?;
        let key = load_or_create_key(&config_dir.join("control-client.key"))?;
        Self::connect_to(server, key).await
    }

    async fn connect_to(server: EndpointAddr, key: SecretKey) -> Result<Self> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(key)
            .address_lookup(iroh::address_lookup::dns::DnsAddressLookup::n0_dns())
            .bind()
            .await?;
        Ok(Self::from_endpoint(endpoint, server))
    }

    pub async fn connect(state_dir: &Path) -> Result<Self> {
        let server = configured_address(state_dir)?;
        let key = load_or_create_key(&state_dir.join("control-client.key"))?;
        if let Some(server) = server {
            Self::connect_to(server, key).await
        } else {
            let addr: EndpointAddr = serde_json::from_slice(&tokio::fs::read(state_dir.join("control.addr")).await.context("daemon is not running: missing control.addr; configure a remote endpoint with --endpoint")?)?;
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(key)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let mut client = Self::from_endpoint(endpoint, addr);
            client.remote = false;
            Ok(client)
        }
    }
    pub async fn get_gateway(&self) -> Result<crate::GatewaySnapshot> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::GetGateway {}),
        )
        .await
        .context("gateway settings request timed out")??
        .map_err(anyhow::Error::msg)
    }
    pub async fn set_gateway(
        &self,
        config: crate::GatewayConfig,
    ) -> Result<crate::GatewaySnapshot> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::SetGateway { config }),
        )
        .await
        .context("gateway settings request timed out; check current settings before retrying")??
        .map_err(anyhow::Error::msg)
    }
    pub async fn create_pairing_ticket(&self) -> Result<crate::PairingTicket> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::CreatePairingTicket {}),
        )
        .await
        .context("pairing ticket request timed out")??
        .map_err(anyhow::Error::msg)
    }

    pub async fn allow_control(&self, endpoint: crate::EndpointId) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::AllowControl { endpoint }),
        )
        .await??
        .map_err(anyhow::Error::msg)
    }
    pub async fn revoke_control(&self, endpoint: crate::EndpointId) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::RevokeControl { endpoint }),
        )
        .await??
        .map_err(anyhow::Error::msg)
    }
    pub async fn list_control(&self) -> Result<Vec<crate::EndpointId>> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::ListControl {}),
        )
        .await??
        .map_err(anyhow::Error::msg)
    }

    pub async fn complete_path(&self, path: PathBuf) -> Result<crate::PathCompletions> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::CompletePath { path }),
        )
        .await
        .context("path completion timed out")??
        .map_err(anyhow::Error::msg)
    }

    pub async fn share(&self, path: PathBuf) -> Result<Job> {
        let path = if self.remote {
            path
        } else {
            tokio::fs::canonicalize(path).await?
        };
        tokio::time::timeout(Duration::from_secs(10), self.client.rpc(Share { path }))
            .await
            .context("share request timed out; check the job list before retrying")??
            .map_err(anyhow::Error::msg)
    }

    pub async fn download(&self, ticket: BlobTicket, target: PathBuf) -> Result<Job> {
        let target = if self.remote {
            target
        } else {
            std::path::absolute(target)?
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(Download { ticket, target }),
        )
        .await
        .context("download request timed out; check the job list before retrying")??
        .map_err(anyhow::Error::msg)
    }

    pub async fn list(&self) -> Result<Vec<Job>> {
        tokio::time::timeout(Duration::from_secs(10), self.client.rpc(List {}))
            .await
            .context("list request timed out")??
            .map_err(anyhow::Error::msg)
    }

    pub async fn remove(&self, id: u64) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), self.client.rpc(Remove { id }))
            .await
            .context("remove request timed out; check the job list before retrying")??
            .map_err(anyhow::Error::msg)
    }

    pub async fn watch(&self) -> Result<mpsc::Receiver<RpcResult<WatchEvent>>> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.server_streaming(Watch {}, 128),
        )
        .await
        .context("watch request timed out")?
        .map_err(Into::into)
    }
}

impl ControlClient {
    pub async fn create_name(
        &self,
        label: String,
        target: crate::NameTarget,
    ) -> Result<crate::Name> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::CreateName { label, target }),
        )
        .await
        .context("create name timed out; check names before retrying")??
        .map_err(anyhow::Error::msg)
    }
    pub async fn update_name(
        &self,
        label: String,
        target: crate::NameTarget,
    ) -> Result<crate::Name> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::UpdateName { label, target }),
        )
        .await
        .context("update name timed out; check names before retrying")??
        .map_err(anyhow::Error::msg)
    }
    pub async fn remove_name(&self, label: String) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::RemoveName { label }),
        )
        .await
        .context("remove name timed out; check names before retrying")??
        .map_err(anyhow::Error::msg)
    }
    pub async fn list_names(&self) -> Result<Vec<crate::Name>> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::ListNames {}),
        )
        .await
        .context("list names timed out")??
        .map_err(anyhow::Error::msg)
    }
}

/// Atomically create a private identity file; concurrent clients use the winner.
pub fn load_or_create_key(path: &Path) -> Result<SecretKey> {
    fn read(path: &Path) -> Result<SecretKey> {
        let bytes: [u8; 32] = std::fs::read(path)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid identity key length"))?;
        Ok(SecretKey::from_bytes(&bytes))
    }
    if path.try_exists()? {
        return read(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", rand::random::<u64>()));
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
        file.write_all(&rand::random::<[u8; 32]>())?;
        file.sync_all()?;
        match std::fs::hard_link(&temporary, path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error.into()),
        }
    })();
    let _ = std::fs::remove_file(temporary);
    result?;
    read(path)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ClientConfig {
    endpoint: crate::EndpointId,
    #[serde(default)]
    addr: Option<EndpointAddr>,
}
pub fn configured_endpoint(root: &Path) -> Result<Option<crate::EndpointId>> {
    Ok(configured_address(root)?.map(|addr| addr.id))
}
pub fn configured_address(root: &Path) -> Result<Option<EndpointAddr>> {
    match std::fs::read(root.join("client.json")) {
        Ok(bytes) => {
            let config: ClientConfig = serde_json::from_slice(&bytes)?;
            let addr = config.addr.unwrap_or_else(|| config.endpoint.into());
            anyhow::ensure!(
                addr.id == config.endpoint,
                "configured endpoint does not match address"
            );
            Ok(Some(addr))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub fn configure_endpoint(root: &Path, endpoint: Option<crate::EndpointId>) -> Result<()> {
    configure_address(root, endpoint.map(EndpointAddr::from))
}
pub fn configure_address(root: &Path, addr: Option<EndpointAddr>) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let path = root.join("client.json");
    if let Some(addr) = addr {
        let temporary = root.join(format!("client.{}.tmp", rand::random::<u64>()));
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&ClientConfig {
                endpoint: addr.id,
                addr: Some(addr),
            })?)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Redeem over an authenticated connection; the server sees this endpoint's identity.
pub async fn redeem_pairing(endpoint: &Endpoint, ticket: &crate::PairingTicket) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let connection = endpoint.connect(ticket.addr.clone(), crate::PAIRING_ALPN).await?;
        let result = async {
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(ticket.secret.as_bytes()).await?;
            send.finish()?;
            let response = recv.read_to_end(1).await?;
            match response.as_slice() {
                [value] if *value == crate::PairingStatus::Accepted as u8 => Ok(()),
                [value] if *value == crate::PairingStatus::StorageFailure as u8 => anyhow::bail!("daemon could not save authorization; retry the ticket"),
                _ => anyhow::bail!("pairing ticket is invalid or already claimed; run blobtorrent control pair for a new ticket"),
            }
        }.await;
        connection.close(0u32.into(), b"pairing complete");
        result
    }).await.context("pairing timed out; retry with the same ticket and config directory")?
}

/// Persist the client identity before enrollment and save only the daemon address.
pub async fn pair(config_dir: &Path, ticket: &crate::PairingTicket) -> Result<()> {
    let key = load_or_create_key(&config_dir.join("control-client.key"))?;
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .address_lookup(iroh::address_lookup::dns::DnsAddressLookup::n0_dns())
        .bind()
        .await?;
    let result = redeem_pairing(&endpoint, ticket).await;
    endpoint.close().await;
    result?;
    configure_address(config_dir, Some(ticket.addr.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn configured_client_never_falls_back_to_daemon_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("control.addr"), b"not a daemon address")?;
        let error = match ControlClient::connect_configured(root.path()).await {
            Ok(_) => anyhow::bail!("connected without a configured endpoint"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("no daemon configured"));
        assert!(!root.path().join("control-client.key").exists());
        Ok(())
    }

    #[test]
    fn identity_and_server_choice_survive_restart() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("control-client.key");
        let first = load_or_create_key(&path)?.public();
        assert_eq!(first, load_or_create_key(&path)?.public());
        assert!(configured_endpoint(root.path())?.is_none());
        configure_endpoint(root.path(), Some(first))?;
        assert_eq!(configured_endpoint(root.path())?, Some(first));
        configure_endpoint(root.path(), None)?;
        assert!(configured_endpoint(root.path())?.is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
        }
        Ok(())
    }
    #[test]
    fn concurrent_identity_creation_never_replaces_a_key() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("key");
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || load_or_create_key(&path).unwrap().public())
            })
            .collect();
        let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(ids.iter().all(|id| *id == ids[0]));
        Ok(())
    }
}
