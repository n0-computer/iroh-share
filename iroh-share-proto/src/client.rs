//! Optional local client support, for native control clients.
use crate::{ControlProtocol, Download, Job, List, Remove, RpcResult, Share, Watch, WatchEvent};
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
        .map(|base| base.join("iroh-share"))
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
        if let Some(server) = configured_address(state_dir)? {
            let key = load_or_create_key(&state_dir.join("control-client.key"))?;
            Self::connect_to(server, key).await
        } else {
            Self::connect_local(state_dir).await
        }
    }
    /// Use the local daemon owner's identity, ignoring configured remote servers.
    /// Intended for local administration and installer setup, not ordinary UI connections.
    pub async fn connect_local(state_dir: &Path) -> Result<Self> {
        let addr: EndpointAddr = serde_json::from_slice(
            &tokio::fs::read(state_dir.join("control.addr"))
                .await
                .context("local daemon is not running: missing control.addr")?,
        )?;
        let key = load_or_create_key(&state_dir.join("control-client.key"))?;
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let mut client = Self::from_endpoint(endpoint, addr);
        client.remote = false;
        Ok(client)
    }
    pub async fn shutdown(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), self.client.rpc(crate::Shutdown {}))
            .await
            .context("daemon shutdown request timed out")??
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
        self.share_with_options(path, false).await
    }

    pub async fn share_with_options(
        &self,
        path: PathBuf,
        include_directory_name: bool,
    ) -> Result<Job> {
        let path = if self.remote {
            path
        } else {
            tokio::fs::canonicalize(path).await?
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(Share {
                path,
                include_directory_name,
            }),
        )
        .await
        .context("share request timed out; check the job list before retrying")??
        .map_err(anyhow::Error::msg)
    }

    pub async fn import(&self, source: crate::DownloadSource, id: Option<u64>) -> Result<Job> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::Import { source, id }),
        )
        .await
        .context("import request timed out; check data before retrying")??
        .map_err(anyhow::Error::msg)
    }

    pub async fn import_directory(&self) -> Result<PathBuf> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::GetImportDirectory {}),
        )
        .await
        .context("import directory request timed out")??
        .map_err(anyhow::Error::msg)
    }

    pub async fn refresh(&self, id: u64) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(crate::Refresh { id }),
        )
        .await
        .context("refresh request timed out; check data before retrying")??
        .map_err(anyhow::Error::msg)
    }

    pub async fn download(
        &self,
        source: impl Into<crate::DownloadSource>,
        target: PathBuf,
    ) -> Result<Job> {
        let target = if self.remote {
            target
        } else {
            std::path::absolute(target)?
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            self.client.rpc(Download {
                source: source.into(),
                target,
            }),
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
    /// Save a private-key backup of all names locally, without overwriting an existing file.
    pub async fn export_names(&self, output: &std::path::Path) -> Result<()> {
        save_private(output, async {
            self.client
                .rpc(crate::ExportNames {})
                .await?
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    /// Save a private-key backup of one name, in the same layout as [`Self::export_names`].
    pub async fn export_name(&self, label: String, output: &std::path::Path) -> Result<()> {
        save_private(output, async {
            self.client
                .rpc(crate::ExportName { label })
                .await?
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    /// Restore names from a local backup archive.
    pub async fn import_names(&self, input: &std::path::Path) -> Result<Vec<crate::ImportedName>> {
        let archive = tokio::fs::read(input)
            .await
            .context("cannot read name archive")?;
        tokio::time::timeout(
            Duration::from_secs(60),
            self.client.rpc(crate::ImportNames { archive }),
        )
        .await
        .context("name import timed out; check names before retrying")??
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
    /// The daemon used most recently, which clients connect to on start.
    endpoint: crate::EndpointId,
    #[serde(default)]
    addr: Option<EndpointAddr>,
    /// Every paired daemon, including the current one. Older files omit it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    daemons: Vec<SavedDaemon>,
}

/// A daemon this client has paired with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedDaemon {
    pub addr: EndpointAddr,
    /// Optional label chosen by the user; clients fall back to the endpoint ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl SavedDaemon {
    pub fn id(&self) -> crate::EndpointId {
        self.addr.id
    }
    /// The user's label, or a shortened endpoint ID.
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(name) => name.clone(),
            None => self.addr.id.fmt_short().to_string(),
        }
    }
}

fn read_config(root: &Path) -> Result<Option<ClientConfig>> {
    match std::fs::read(root.join("client.json")) {
        Ok(bytes) => {
            let mut config: ClientConfig = serde_json::from_slice(&bytes)?;
            let addr = config
                .addr
                .clone()
                .unwrap_or_else(|| config.endpoint.into());
            anyhow::ensure!(
                addr.id == config.endpoint,
                "configured endpoint does not match address"
            );
            if !config.daemons.iter().any(|d| d.id() == addr.id) {
                config.daemons.insert(0, SavedDaemon { addr, name: None });
            }
            Ok(Some(config))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_config(root: &Path, config: Option<&ClientConfig>) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let path = root.join("client.json");
    let Some(config) = config else {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        };
    };
    let temporary = root.join(format!("client.{}.tmp", rand::random::<u64>()));
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(config)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub fn configured_endpoint(root: &Path) -> Result<Option<crate::EndpointId>> {
    Ok(configured_address(root)?.map(|addr| addr.id))
}
pub fn configured_address(root: &Path) -> Result<Option<EndpointAddr>> {
    Ok(read_config(root)?.map(|config| {
        config
            .daemons
            .into_iter()
            .find(|d| d.id() == config.endpoint)
            .expect("current daemon is listed")
            .addr
    }))
}
pub fn configure_endpoint(root: &Path, endpoint: Option<crate::EndpointId>) -> Result<()> {
    configure_address(root, endpoint.map(EndpointAddr::from))
}
/// Make `addr` the current daemon, adding it to the saved list if needed.
///
/// `None` forgets every saved daemon.
pub fn configure_address(root: &Path, addr: Option<EndpointAddr>) -> Result<()> {
    let Some(addr) = addr else {
        return write_config(root, None);
    };
    let mut daemons = read_config(root)?
        .map(|config| config.daemons)
        .unwrap_or_default();
    match daemons.iter_mut().find(|d| d.id() == addr.id) {
        // A bare endpoint ID must not discard address hints learned from a ticket.
        Some(_) if addr.is_empty() => {}
        Some(saved) => saved.addr = addr.clone(),
        None => daemons.push(SavedDaemon {
            addr: addr.clone(),
            name: None,
        }),
    }
    let addr = daemons
        .iter()
        .find(|d| d.id() == addr.id)
        .expect("just added")
        .addr
        .clone();
    write_config(
        root,
        Some(&ClientConfig {
            endpoint: addr.id,
            addr: Some(addr),
            daemons,
        }),
    )
}

/// Every daemon this client has paired with, in the order they were added.
pub fn saved_daemons(root: &Path) -> Result<Vec<SavedDaemon>> {
    Ok(read_config(root)?
        .map(|config| config.daemons)
        .unwrap_or_default())
}

/// Switch to a saved daemon; clients connect to it from now on.
pub fn select_daemon(root: &Path, id: crate::EndpointId) -> Result<()> {
    let config = read_config(root)?.context("no daemons are saved")?;
    let addr = config
        .daemons
        .iter()
        .find(|d| d.id() == id)
        .context("this daemon is not saved")?
        .addr
        .clone();
    configure_address(root, Some(addr))
}

/// Set or clear a saved daemon's label.
pub fn rename_daemon(root: &Path, id: crate::EndpointId, name: Option<String>) -> Result<()> {
    let mut config = read_config(root)?.context("no daemons are saved")?;
    let saved = config
        .daemons
        .iter_mut()
        .find(|d| d.id() == id)
        .context("this daemon is not saved")?;
    saved.name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    write_config(root, Some(&config))
}

/// Remove a saved daemon. If it was current, the first remaining daemon becomes
/// current, which is returned; `None` means no daemons are left.
///
/// The daemon keeps this client authorized; pairing again needs a new ticket.
pub fn forget_daemon(root: &Path, id: crate::EndpointId) -> Result<Option<crate::EndpointId>> {
    let Some(mut config) = read_config(root)? else {
        return Ok(None);
    };
    config.daemons.retain(|d| d.id() != id);
    let Some(current) = config
        .daemons
        .iter()
        .find(|d| d.id() == config.endpoint)
        .or_else(|| config.daemons.first())
        .cloned()
    else {
        write_config(root, None)?;
        return Ok(None);
    };
    config.endpoint = current.id();
    config.addr = Some(current.addr);
    write_config(root, Some(&config))?;
    Ok(Some(config.endpoint))
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
                _ => anyhow::bail!("pairing ticket is invalid or already claimed; run iroh-share control pair for a new ticket"),
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

/// Writes private key material to a new owner-only file, removing it on failure.
async fn save_private(
    output: &std::path::Path,
    bytes: impl std::future::Future<Output = Result<Vec<u8>>>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(output)
        .context("cannot create export file (choose a new filename)")?;
    let mut file = tokio::fs::File::from_std(file);
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        let bytes = bytes.await?;
        file.write_all(&bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("name export timed out")
    .and_then(|result| result);
    drop(file);
    if result.is_err() {
        let _ = tokio::fs::remove_file(output).await;
    }
    result
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
    fn saved_daemons_switch_rename_and_forget() -> Result<()> {
        let root = tempfile::tempdir()?;
        let id = |n: u8| SecretKey::from_bytes(&[n; 32]).public();
        let (a, b) = (id(1), id(2));
        // Files from before the list existed read as one saved daemon, unchanged.
        let legacy = serde_json::to_vec_pretty(&serde_json::json!({ "endpoint": a }))?;
        std::fs::write(root.path().join("client.json"), &legacy)?;
        let daemons = saved_daemons(root.path())?;
        assert_eq!(daemons.len(), 1);
        assert_eq!(daemons[0].id(), a);
        assert_eq!(std::fs::read(root.path().join("client.json"))?, legacy);

        let hinted = EndpointAddr::new(b).with_ip_addr("127.0.0.1:4433".parse()?);
        configure_address(root.path(), Some(hinted.clone()))?;
        assert_eq!(configured_endpoint(root.path())?, Some(b));
        // Selecting by bare ID keeps the address hints from pairing.
        configure_endpoint(root.path(), Some(b))?;
        assert_eq!(configured_address(root.path())?, Some(hinted.clone()));
        select_daemon(root.path(), a)?;
        assert_eq!(configured_endpoint(root.path())?, Some(a));
        assert!(select_daemon(root.path(), id(3)).is_err());

        rename_daemon(root.path(), b, Some("  nas  ".into()))?;
        let daemons = saved_daemons(root.path())?;
        assert_eq!(
            daemons.iter().map(SavedDaemon::id).collect::<Vec<_>>(),
            [a, b]
        );
        assert_eq!(daemons[1].display_name(), "nas");
        assert_eq!(daemons[1].addr, hinted);
        rename_daemon(root.path(), b, Some(" ".into()))?;
        assert_eq!(saved_daemons(root.path())?[1].name, None);

        // Forgetting another daemon keeps the current one.
        assert_eq!(forget_daemon(root.path(), b)?, Some(a));
        configure_address(root.path(), Some(hinted))?;
        // Forgetting the current daemon switches to one that remains.
        assert_eq!(forget_daemon(root.path(), b)?, Some(a));
        assert_eq!(configured_endpoint(root.path())?, Some(a));
        assert_eq!(forget_daemon(root.path(), a)?, None);
        assert!(saved_daemons(root.path())?.is_empty());
        assert!(!root.path().join("client.json").exists());
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
