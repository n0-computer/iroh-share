mod control;
mod dns_records;
mod gateway;
mod names;
mod paths;
mod recovery;
mod transfer;
mod uploads;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use iroh::{endpoint::presets, protocol::Router, Endpoint};
use iroh_blobs::{store::fs::FsStore, BlobsProtocol, Hash};
use iroh_share_proto::{
    ControlMessage, DiscoveryMode, DownloadSource, Job, JobKind, JobState, NameTarget, RpcResult,
    Url, WatchEvent,
};
use irpc::WithChannels;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[derive(Parser)]
struct Args {
    /// State directory (defaults to the platform's per-user iroh-share directory).
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: CommandLine,
}

#[derive(Subcommand)]
enum CommandLine {
    Daemon {
        /// Disable public Mainline announcements; direct tickets still work.
        #[arg(long)]
        no_announce: bool,
        /// Do not print a first-start invitation (for background launchers).
        #[arg(long)]
        no_pairing_ticket: bool,
        /// Public folder for ticket imports (defaults to Downloads/Iroh Share).
        #[arg(long)]
        import_dir: Option<PathBuf>,
    },
    Share {
        path: PathBuf,
        /// Include the directory basename in collection paths.
        #[arg(long)]
        include_directory_name: bool,
    },
    Download {
        /// Collection blake3.net URL, z32/hex hash, or blob ticket.
        source: DownloadSource,
        target: PathBuf,
        /// Also discover providers if the supplied ticket's provider cannot complete the download.
        #[arg(long)]
        discover: bool,
    },
    List,
    /// Rescan a local directory followed by a managed name.
    Refresh {
        id: u64,
    },
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    Names {
        #[command(subcommand)]
        command: NameCommand,
    },
    Watch,
    Remove {
        id: u64,
    },
}

#[derive(Subcommand)]
enum ControlCommand {
    /// Gracefully stop the daemon.
    Stop,
    /// Print a one-client pairing ticket for the TUI.
    Pair,
    Id,
    Endpoint,
    List,
    Allow {
        endpoint: iroh_share_proto::EndpointId,
    },
    Revoke {
        endpoint: iroh_share_proto::EndpointId,
    },
}

#[derive(Subcommand)]
enum NameCommand {
    /// Export all pkarr keys and signed records to a new ZIP file on this computer.
    Export {
        output: PathBuf,
    },
    /// Export one name's pkarr key and signed record to a new ZIP file on this computer.
    ExportName {
        label: String,
        output: PathBuf,
    },
    /// Restore names from a ZIP written by `export` or `export-name`.
    Import {
        archive: PathBuf,
    },
    Create {
        label: String,
        #[command(flatten)]
        target: TargetArgs,
    },
    Update {
        label: String,
        #[command(flatten)]
        target: TargetArgs,
    },
    List,
    Remove {
        label: String,
    },
}
#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct TargetArgs {
    #[arg(long)]
    url: Option<Url>,
    #[arg(long)]
    job: Option<u64>,
}
impl TargetArgs {
    fn into_target(self) -> NameTarget {
        match self.url {
            Some(url) => NameTarget::Url(url),
            None => NameTarget::Job(self.job.expect("clap requires a target")),
        }
    }
}

struct Actor {
    rx: mpsc::Receiver<ControlMessage>,
    jobs: BTreeMap<u64, Job>,
    tasks: BTreeMap<u64, tokio::task::JoinHandle<()>>,
    names: names::Names,
    checkpoints_tx: mpsc::Sender<recovery::Checkpoint>,
    checkpoints_rx: mpsc::Receiver<recovery::Checkpoint>,
    name_updates_rx: mpsc::Receiver<names::Published>,
    store: FsStore,
    endpoint: Endpoint,
    access: control::Access,
    pairing: control::Pairing,
    gateway: gateway::Manager,
    watchers: Vec<irpc::channel::mpsc::Sender<RpcResult<WatchEvent>>>,
    refreshes: BTreeMap<u64, mpsc::Sender<()>>,
    imports_dir: PathBuf,
    updates_tx: mpsc::Sender<Job>,
    updates_rx: mpsc::Receiver<Job>,
    uploads: watch::Receiver<BTreeMap<Hash, u32>>,
    announcements: watch::Sender<std::collections::HashSet<Hash>>,
}

impl Actor {
    async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let mut upload_tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            let message = tokio::select! {
                _ = shutdown.changed() => break,
                _ = upload_tick.tick() => {
                    let mut changed = Vec::new();
                    for job in self.jobs.values_mut() {
                        if let JobState::Seeding { ticket, active_uploads } = &mut job.state {
                            let count = self.uploads.borrow().get(&ticket.hash()).copied().unwrap_or(0);
                            if *active_uploads != count {
                                *active_uploads = count;
                                changed.push(job.clone());
                            }
                        }
                    }
                    for job in changed {
                        self.broadcast(WatchEvent::JobUpdated(Box::new(job))).await;
                    }
                    continue;
                }
                result = self.gateway.updates.changed() => {
                    if result.is_err() { break; }
                    self.broadcast(WatchEvent::GatewayUpdated(self.gateway.snapshot())).await;
                    continue;
                }
                message = self.rx.recv() => match message { Some(message) => message, None => break },
                Some(checkpoint) = self.checkpoints_rx.recv() => {
                    let result = self.names.checkpoint(checkpoint.id, checkpoint.phase).map_err(|e| e.to_string());
                    let _ = checkpoint.ack.send(result);
                    continue;
                }
                Some(update) = self.name_updates_rx.recv() => {
                    if let Some(name) = self.names.published(update) {
                        self.broadcast(WatchEvent::NameUpdated(Box::new(name))).await;
                    }
                    continue;
                }
                update = self.updates_rx.recv() => {
                    if let Some(job) = update {
                        if let std::collections::btree_map::Entry::Occupied(mut entry) = self.jobs.entry(job.id) {
                            entry.insert(job.clone());
                            self.refresh_announcements();
                            self.refresh_names().await;
                            self.broadcast(WatchEvent::JobUpdated(Box::new(job))).await;
                        }
                    }
                    continue;
                }
            };
            match message {
                ControlMessage::Shutdown(message) => {
                    let _ = message.tx.send(Ok(())).await;
                    break;
                }
                ControlMessage::GetGateway(message) => {
                    let _ = message.tx.send(Ok(self.gateway.snapshot())).await;
                }
                ControlMessage::SetGateway(message) => {
                    let controller = self.gateway.controller();
                    tokio::spawn(async move {
                        let result = controller
                            .set(message.inner.config)
                            .await
                            .map_err(|error| format!("{error:#}"));
                        let _ = message.tx.send(result).await;
                    });
                }
                ControlMessage::CreatePairingTicket(message) => {
                    let ticket = self.pairing.issue(control::pairing_address(&self.endpoint));
                    let _ = message.tx.send(Ok(ticket)).await;
                }
                ControlMessage::CompletePath(message) => {
                    // Directory I/O must not hold up Watch events or other control requests.
                    tokio::spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            paths::complete(&message.inner.path)
                        })
                        .await
                        .map_err(anyhow::Error::from)
                        .and_then(|result| result)
                        .map_err(|error| format!("{error:#}"));
                        let _ = message.tx.send(result).await;
                    });
                }
                ControlMessage::AllowControl(message) => {
                    let result = self
                        .access
                        .update(message.inner.endpoint, true)
                        .map_err(|e| e.to_string());
                    let _ = message.tx.send(result).await;
                }
                ControlMessage::RevokeControl(message) => {
                    let result = self
                        .access
                        .update(message.inner.endpoint, false)
                        .map_err(|e| e.to_string());
                    let _ = message.tx.send(result).await;
                }
                ControlMessage::ListControl(message) => {
                    let _ = message.tx.send(Ok(self.access.list())).await;
                }

                ControlMessage::CreateName(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = {
                        self.names
                            .set(inner.label, inner.target, true, &self.jobs)
                            .map_err(|e| e.to_string())
                    };
                    if let Ok(name) = &result {
                        self.broadcast(WatchEvent::NameUpdated(Box::new(name.clone())))
                            .await;
                    }
                    let _ = tx.send(result).await;
                }
                ControlMessage::UpdateName(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = {
                        self.names
                            .set(inner.label, inner.target, false, &self.jobs)
                            .map_err(|e| e.to_string())
                    };
                    if let Ok(name) = &result {
                        self.broadcast(WatchEvent::NameUpdated(Box::new(name.clone())))
                            .await;
                    }
                    let _ = tx.send(result).await;
                }
                ControlMessage::ExportNames(message) => {
                    let result = self.names.export_zip().map_err(|e| e.to_string());
                    let _ = message.tx.send(result).await;
                }
                ControlMessage::ExportName(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = self
                        .names
                        .export_name(&inner.label)
                        .map_err(|e| e.to_string());
                    let _ = tx.send(result).await;
                }
                ControlMessage::ImportNames(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = self
                        .names
                        .import_zip(&inner.archive, &self.jobs)
                        .map_err(|e| format!("{e:#}"));
                    if let Ok(outcomes) = &result {
                        for name in self.names.list() {
                            let imported = outcomes.iter().any(|o| {
                                matches!(&o.outcome, iroh_share_proto::ImportOutcome::Imported { label } if *label == name.label)
                            });
                            if imported {
                                self.broadcast(WatchEvent::NameUpdated(Box::new(name)))
                                    .await;
                            }
                        }
                    }
                    let _ = tx.send(result).await;
                }
                ControlMessage::ListNames(message) => {
                    let result = { Ok(self.names.list()) };
                    let _ = message.tx.send(result).await;
                }
                ControlMessage::RemoveName(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = {
                        self.names
                            .remove(&inner.label, &self.jobs)
                            .map_err(|e| e.to_string())
                    };
                    if result.is_ok() {
                        self.broadcast(WatchEvent::NameRemoved { label: inner.label })
                            .await;
                    }
                    let _ = tx.send(result).await;
                }
                ControlMessage::GetImportDirectory(message) => {
                    let _ = message.tx.send(Ok(self.imports_dir.clone())).await;
                }
                ControlMessage::Import(message) => {
                    let result = self
                        .import_ticket(message.inner.source, message.inner.id)
                        .await;
                    if let Ok(job) = &result {
                        self.refresh_announcements();
                        self.refresh_names().await;
                        self.broadcast(WatchEvent::JobUpdated(Box::new(job.clone())))
                            .await;
                    }
                    let _ = message.tx.send(result).await;
                }
                ControlMessage::Refresh(message) => {
                    let result = self.request_refresh(message.inner.id);
                    let _ = message.tx.send(result).await;
                }
                ControlMessage::Share(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = {
                        self.start(JobKind::Share {
                            path: inner.path,
                            include_directory_name: inner.include_directory_name,
                        })
                    };
                    if let Ok(job) = &result {
                        self.broadcast(WatchEvent::JobUpdated(Box::new(job.clone())))
                            .await;
                    }
                    let _ = tx.send(result).await;
                }
                ControlMessage::Download(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = {
                        self.start(JobKind::Download {
                            source: inner.source,
                            target: inner.target,
                        })
                    };
                    if let Ok(job) = &result {
                        self.broadcast(WatchEvent::JobUpdated(Box::new(job.clone())))
                            .await;
                    }
                    let _ = tx.send(result).await;
                }
                ControlMessage::List(message) => {
                    let jobs = { Ok(self.jobs.values().cloned().collect()) };
                    let _ = message.tx.send(jobs).await;
                }
                ControlMessage::Remove(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    if self.jobs.contains_key(&inner.id) {
                        if let Err(error) = self.names.forget_job(inner.id) {
                            let _ = tx.send(Err(error.to_string())).await;
                            continue;
                        }
                    }
                    let result = if self.jobs.remove(&inner.id).is_some() {
                        self.refreshes.remove(&inner.id);
                        if let Some(task) = self.tasks.remove(&inner.id) {
                            task.abort();
                            let _ = task.await;
                        }
                        if let Err(error) = self
                            .store
                            .tags()
                            .delete(format!("iroh-share/data/{}", inner.id))
                            .await
                        {
                            tracing::warn!(%error, "Cannot remove persistent data tag");
                        }
                        if let Err(error) = self
                            .store
                            .tags()
                            .delete(format!("iroh-share/previous/{}", inner.id))
                            .await
                        {
                            tracing::warn!(%error, "Cannot remove previous content tag");
                        }
                        self.refresh_announcements();
                        self.refresh_names().await;
                        self.broadcast(WatchEvent::JobRemoved { id: inner.id })
                            .await;
                        Ok(())
                    } else {
                        Err(format!("unknown job {}", inner.id))
                    };
                    let _ = tx.send(result).await;
                }
                ControlMessage::Watch(message) => {
                    let tx = message.tx;
                    let mut alive = true;
                    for job in self.jobs.values() {
                        if !send_update(&tx, WatchEvent::JobUpdated(Box::new(job.clone()))).await {
                            alive = false;
                            break;
                        }
                    }
                    if alive {
                        for name in self.names.list() {
                            if !send_update(&tx, WatchEvent::NameUpdated(Box::new(name))).await {
                                alive = false;
                                break;
                            }
                        }
                    }
                    if alive {
                        alive =
                            send_update(&tx, WatchEvent::GatewayUpdated(self.gateway.snapshot()))
                                .await;
                    }
                    if alive && send_update(&tx, WatchEvent::SnapshotComplete).await {
                        self.watchers.push(tx);
                    }
                }
            }
        }
        for task in self.tasks.values() {
            task.abort();
        }
        for (_, task) in self.tasks {
            let _ = task.await;
        }
        self.gateway.shutdown().await;
    }

    async fn refresh_names(&mut self) {
        match self.names.refresh(&self.jobs) {
            Ok(names) => {
                for name in names {
                    self.broadcast(WatchEvent::NameUpdated(Box::new(name)))
                        .await;
                }
            }
            Err(error) => tracing::error!(%error, "Cannot update pkarr names"),
        }
    }

    fn refresh_announcements(&self) {
        self.announcements.send_replace(
            self.jobs
                .values()
                .filter_map(|job| match &job.state {
                    JobState::Seeding { ticket, .. } => Some(ticket.hash()),
                    _ => None,
                })
                .collect(),
        );
    }

    async fn broadcast(&mut self, event: WatchEvent) {
        let mut active = Vec::new();
        for watcher in self.watchers.drain(..) {
            if send_update(&watcher, event.clone()).await {
                active.push(watcher);
            }
        }
        self.watchers = active;
    }

    async fn import_ticket(&mut self, source: DownloadSource, id: Option<u64>) -> RpcResult<Job> {
        source.validate().map_err(|e| e.to_string())?;
        if let Some(id) = id {
            let job = self
                .jobs
                .get(&id)
                .ok_or("This data is no longer available")?;
            if !matches!(
                job.state,
                JobState::Seeding { .. } | JobState::Failed { .. }
            ) {
                return Err("Wait for the current transfer to finish before updating".into());
            }
        }
        // A fresh destination preserves user files and permits changed collection layouts.
        let target = self.imports_dir.join(format!(
            "{}-{}",
            &source.hash.to_hex()[..12],
            hex::encode(rand::random::<[u8; 4]>())
        ));
        std::fs::create_dir_all(&self.imports_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir(&target).map_err(|e| e.to_string())?;
        let target = target.canonicalize().map_err(|e| e.to_string())?;
        let kind = JobKind::Download { source, target };
        let Some(id) = id else {
            return self.start(kind);
        };
        // Keep the previous collection rooted while the replacement is fetched.
        if let JobState::Seeding { ticket, .. } = &self.jobs[&id].state {
            self.store
                .tags()
                .set(
                    format!("iroh-share/previous/{id}"),
                    iroh_blobs::HashAndFormat::new(ticket.hash(), ticket.format()),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        self.names
            .replace_job(id, &kind)
            .map_err(|e| e.to_string())?;
        if let Some(task) = self.tasks.remove(&id) {
            task.abort();
            let _ = task.await;
        }
        // The stopped worker can have buffered progress; don't apply it to its replacement.
        while let Ok(job) = self.updates_rx.try_recv() {
            if job.id != id && self.jobs.contains_key(&job.id) {
                self.jobs.insert(job.id, job.clone());
                self.broadcast(WatchEvent::JobUpdated(Box::new(job))).await;
            }
        }
        Ok(self.spawn_job(id, kind))
    }

    fn request_refresh(&self, id: u64) -> RpcResult<()> {
        let job = self
            .jobs
            .get(&id)
            .ok_or("This data is no longer available")?;
        let JobKind::Share { path, .. } = &job.kind else {
            return Err("Only shared local directories can be refreshed".into());
        };
        if !path.is_dir() {
            return Err("The shared path must be an existing directory".into());
        }
        if !self
            .names
            .list()
            .iter()
            .any(|name| name.target == NameTarget::Job(id))
        {
            return Err("Create a name that follows this directory before refreshing".into());
        }
        if !matches!(
            job.state,
            JobState::Seeding { .. } | JobState::Failed { .. }
        ) {
            return Err("Wait for the current import to finish".into());
        }
        self.refreshes
            .get(&id)
            .ok_or("The share worker is unavailable")?
            .try_send(())
            .map_err(|_| "A refresh is already pending or the share worker stopped".into())
    }

    fn start(&mut self, mut kind: JobKind) -> RpcResult<Job> {
        let path = match &mut kind {
            JobKind::Share { path, .. } => path,
            JobKind::Download { target, .. } => target,
        };
        *path = paths::resolve(path).map_err(|e| e.to_string())?;
        match &mut kind {
            JobKind::Share { path, .. } => {
                *path = path.canonicalize().map_err(|e| e.to_string())?;
                if !path.is_file() && !path.is_dir() {
                    return Err("expected a file or directory".into());
                }
            }
            JobKind::Download { source, target } => {
                source.validate().map_err(|error| error.to_string())?;
                std::fs::create_dir_all(&*target).map_err(|e| e.to_string())?;
                *target = target.canonicalize().map_err(|e| e.to_string())?;
                // Avoid two jobs racing to export into overlapping target directories.
                if self.jobs.values().any(|job| matches!(&job.kind, JobKind::Download { target: existing, .. } if target.starts_with(existing) || existing.starts_with(&*target))) {
                    return Err("download target overlaps an existing job; remove that job first".into());
                }
            }
        }
        let id = self.names.allocate_job(&kind).map_err(|e| e.to_string())?;
        Ok(self.spawn_job(id, kind))
    }

    fn spawn_job(&mut self, id: u64, kind: JobKind) -> Job {
        self.spawn_saved(id, recovery::SavedData::new(&kind))
    }
    fn spawn_saved(&mut self, id: u64, saved: recovery::SavedData) -> Job {
        let phase = saved.phase();
        let kind = saved.kind();
        let job = Job {
            id,
            kind,
            state: JobState::Queued,
        };
        let (refresh_tx, refresh_rx) = mpsc::channel(1);
        self.refreshes.insert(id, refresh_tx);
        self.tasks.insert(
            id,
            tokio::spawn(transfer::run_refreshable(
                self.store.as_ref().clone(),
                self.endpoint.clone(),
                job.clone(),
                self.updates_tx.clone(),
                phase,
                Some(self.checkpoints_tx.clone()),
                refresh_rx,
            )),
        );
        self.jobs.insert(id, job.clone());
        job
    }
}

async fn send_update(
    tx: &irpc::channel::mpsc::Sender<RpcResult<WatchEvent>>,
    event: WatchEvent,
) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(100), tx.send(Ok(event))).await,
        Ok(Ok(()))
    )
}

async fn daemon(
    state_dir: &Path,
    no_announce: bool,
    print_pairing_ticket: bool,
    import_dir: Option<PathBuf>,
) -> Result<()> {
    let imports_dir = match import_dir {
        Some(path) => paths::resolve(&path)?,
        None => dirs::download_dir()
            .or_else(dirs::home_dir)
            .context("cannot determine public import directory; provide --import-dir")?
            .join("Iroh Share"),
    };
    tracing::info!(path = %imports_dir.display(), "Ticket import directory");
    tokio::fs::create_dir_all(state_dir).await?;
    // Keep this file handle alive until shutdown. The OS releases the lock even
    // after a crash; a second daemon must fail before changing credentials.
    let daemon_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_dir.join("daemon.lock"))?;
    daemon_lock
        .try_lock()
        .context("another daemon is using this state directory")?;
    let store = FsStore::load(state_dir.join("blobs")).await?;
    let owner =
        iroh_share_proto::client::load_or_create_key(&state_dir.join("control-client.key"))?;
    let access = control::Access::load(state_dir, owner.public())?;
    let first_start = !state_dir.join("daemon.key").try_exists()?;
    let pairing = control::Pairing::default();
    let server_key = iroh_share_proto::client::load_or_create_key(&state_dir.join("daemon.key"))?;
    let blob_endpoint = Endpoint::builder(presets::N0)
        .secret_key(server_key)
        .address_lookup(iroh::address_lookup::pkarr::PkarrPublisher::n0_dns())
        .address_lookup(iroh::address_lookup::dns::DnsAddressLookup::n0_dns())
        .bind()
        .await?;
    let (tx, rx) = mpsc::channel(64);
    let (upload_events, uploads) = uploads::track();
    let router = Router::builder(blob_endpoint.clone())
        .accept(
            iroh_share_proto::PAIRING_ALPN,
            control::PairingProtocol {
                access: access.clone(),
                pairing: pairing.clone(),
            },
        )
        .accept(
            iroh_blobs::ALPN,
            BlobsProtocol::new(&store, Some(upload_events)),
        )
        .accept(
            iroh_share_proto::CONTROL_ALPN,
            control::Control::new(access.clone(), irpc::LocalSender::from(tx)),
        )
        .spawn();
    let mut local_addr = iroh::EndpointAddr::from(blob_endpoint.id());
    for socket in blob_endpoint.bound_sockets() {
        let address = std::net::SocketAddr::new(
            if socket.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            },
            socket.port(),
        );
        local_addr = local_addr.with_ip_addr(address);
    }
    tokio::fs::write(
        state_dir.join("control.addr"),
        serde_json::to_vec(&local_addr)?,
    )
    .await?;
    tracing::info!(endpoint = %blob_endpoint.id(), "Blob and control endpoint ready");
    let (updates_tx, updates_rx) = mpsc::channel(128);
    let (announcements, hashes) = watch::channel(Default::default());
    let dht = if no_announce {
        None
    } else {
        // A random port, not Mainline's default 6881: behind a port-preserving
        // NAT, two machines on 6881 share one public socket and overwrite each
        // other's address-index record.
        Some(n0_mainline::Dht::builder().port(0).build()?)
    };
    let (names, name_publications) = names::Names::load(state_dir, !no_announce)?;
    let restored_data = names.restored_data();
    let (name_updates_tx, name_updates_rx) = mpsc::channel(128);
    let name_publisher = dht
        .clone()
        .map(|dht| tokio::spawn(names::publish(dht, name_publications, name_updates_tx)));
    let announcer = if no_announce {
        None
    } else {
        Some(tokio::spawn(transfer::announce(
            dht.unwrap(),
            blob_endpoint.secret_key().clone(),
            hashes,
        )))
    };
    let (checkpoints_tx, checkpoints_rx) = mpsc::channel(64);
    let mut actor = Actor {
        rx,
        jobs: BTreeMap::new(),
        tasks: BTreeMap::new(),
        names,
        checkpoints_tx,
        checkpoints_rx,
        name_updates_rx,
        store: store.clone(),
        endpoint: blob_endpoint,
        access,
        pairing,
        gateway: gateway::Manager::load(state_dir)?,
        watchers: Vec::new(),
        refreshes: BTreeMap::new(),
        imports_dir,
        updates_tx,
        updates_rx,
        announcements,
        uploads,
    };
    for (id, saved) in restored_data {
        actor.spawn_saved(id, saved);
    }
    actor.refresh_names().await;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server_id = actor.endpoint.id();
    println!("iroh-share endpoint {server_id}");
    if first_start && print_pairing_ticket {
        // Gather relay hints without requiring Internet access for local setup.
        let _ = tokio::time::timeout(Duration::from_secs(5), actor.endpoint.online()).await;
        let ticket = actor
            .pairing
            .issue(control::pairing_address(&actor.endpoint));
        println!("Connect the TUI with this one-client ticket:\niroh-share-tui '{ticket}'");
    }
    let mut actor_task = tokio::spawn(actor.run(shutdown_rx));
    tokio::select! {
        _ = shutdown_signal() => {},
        result = &mut actor_task => { result?; },
    }
    let _ = shutdown_tx.send(true);
    if !actor_task.is_finished() {
        actor_task.await?;
    }
    if let Some(task) = announcer {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = name_publisher {
        task.abort();
        let _ = task.await;
    }
    router.shutdown().await?;
    tokio::fs::remove_file(state_dir.join("control.addr")).await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn client(state_dir: &Path, command: CommandLine) -> Result<()> {
    if matches!(
        command,
        CommandLine::Control {
            command: ControlCommand::Id
        }
    ) {
        println!(
            "{}",
            iroh_share_proto::client::load_or_create_key(&state_dir.join("control-client.key"))?
                .public()
        );
        return Ok(());
    }
    if matches!(
        command,
        CommandLine::Control {
            command: ControlCommand::Endpoint
        }
    ) {
        let addr: iroh::EndpointAddr =
            serde_json::from_slice(&std::fs::read(state_dir.join("control.addr"))?)?;
        println!("{}", addr.id);
        return Ok(());
    }
    let client = iroh_share_proto::client::ControlClient::connect(state_dir).await?;
    match command {
        CommandLine::Control { command } => match command {
            ControlCommand::Id | ControlCommand::Endpoint => unreachable!(),
            ControlCommand::Stop => client.shutdown().await?,
            ControlCommand::Pair => println!("{}", client.create_pairing_ticket().await?),
            ControlCommand::Allow { endpoint } => client.allow_control(endpoint).await?,
            ControlCommand::Revoke { endpoint } => client.revoke_control(endpoint).await?,
            ControlCommand::List => {
                for endpoint in client.list_control().await? {
                    println!("{endpoint}");
                }
            }
        },
        CommandLine::Refresh { id } => {
            client.refresh(id).await?;
            println!("Refresh requested");
        }
        CommandLine::Share {
            path,
            include_directory_name,
        } => println!(
            "{:#?}",
            client
                .share_with_options(path, include_directory_name)
                .await?
        ),
        CommandLine::Download {
            mut source,
            target,
            discover,
        } => {
            if discover {
                source.discovery = DiscoveryMode::Mainline;
            }
            println!("{:#?}", client.download(source, target).await?)
        }
        CommandLine::List => {
            for job in client.list().await? {
                println!("{job:#?}");
            }
        }
        CommandLine::Watch => {
            let mut updates = client.watch().await?;
            while let Some(event) = updates.recv().await? {
                println!("{:#?}", event.map_err(anyhow::Error::msg)?);
            }
        }
        CommandLine::Remove { id } => client.remove(id).await?,
        CommandLine::Names { command } => match command {
            NameCommand::Create { label, target } => {
                let name = client.create_name(label, target.into_target()).await?;
                println!("{}  {}\n{:#?}", name.label, name.key.url(), name);
            }
            NameCommand::Update { label, target } => {
                let name = client.update_name(label, target.into_target()).await?;
                println!("{}  {}\n{:#?}", name.label, name.key.url(), name);
            }
            NameCommand::Export { output } => {
                client.export_names(&output).await?;
                println!("Exported pkarr names to {}", output.display());
            }
            NameCommand::ExportName { label, output } => {
                client.export_name(label, &output).await?;
                println!("Exported pkarr name to {}", output.display());
            }
            NameCommand::Import { archive } => {
                for imported in client.import_names(&archive).await? {
                    match imported.outcome {
                        iroh_share_proto::ImportOutcome::Imported { label } => {
                            println!("{}  imported as {label}", imported.key)
                        }
                        iroh_share_proto::ImportOutcome::Skipped { reason } => {
                            println!("{}  skipped: {reason}", imported.key)
                        }
                    }
                }
            }
            NameCommand::List => {
                for name in client.list_names().await? {
                    println!("{}  {}\n{:#?}", name.label, name.key.url(), name);
                }
            }
            NameCommand::Remove { label } => client.remove_name(label).await?,
        },
        CommandLine::Daemon { .. } => unreachable!(),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,iroh_share=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let state_dir = match args.state_dir {
        Some(path) => path,
        None => iroh_share_proto::client::default_state_dir()?,
    };
    match args.command {
        CommandLine::Daemon {
            no_announce,
            no_pairing_ticket,
            import_dir,
        } => daemon(&state_dir, no_announce, !no_pairing_ticket, import_dir).await,
        command => client(&state_dir, command).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_share_proto::{ControlProtocol, Download, List, Remove, Share, Watch};

    #[tokio::test]
    async fn control_jobs_share_one_store_and_remove_without_deleting_files() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(15), control_lifecycle()).await??;
        Ok(())
    }

    async fn control_lifecycle() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("hello.txt");
        std::fs::write(&source, b"hello")?;
        let store = FsStore::load(temp.path().join("store")).await?;
        let endpoint = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let (tx, rx) = mpsc::channel(64);
        let control_client = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let access = control::Access::load(temp.path(), control_client.id())?;
        let client = irpc_iroh::client::<ControlProtocol>(
            control_client.clone(),
            endpoint.addr(),
            iroh_share_proto::CONTROL_ALPN,
        );
        let (upload_events, uploads) = uploads::track();
        let router = Router::builder(endpoint.clone())
            .accept(
                iroh_blobs::ALPN,
                BlobsProtocol::new(&store, Some(upload_events)),
            )
            .accept(
                iroh_share_proto::CONTROL_ALPN,
                control::Control::new(access.clone(), irpc::LocalSender::from(tx)),
            )
            .spawn();
        let (updates_tx, updates_rx) = mpsc::channel(128);
        let (announcements, hashes) = watch::channel(Default::default());
        let (names, _) = names::Names::load(temp.path(), false)?;
        let (_name_updates_tx, name_updates_rx) = mpsc::channel(128);
        let (checkpoints_tx, checkpoints_rx) = mpsc::channel(64);
        std::fs::write(
            temp.path().join("gateway.json"),
            serde_json::to_vec(&iroh_share_proto::GatewayConfig {
                enabled: false,
                ..Default::default()
            })?,
        )?;
        let actor = Actor {
            rx,
            jobs: BTreeMap::new(),
            tasks: BTreeMap::new(),
            names,
            checkpoints_tx,
            checkpoints_rx,
            name_updates_rx,
            store: store.clone(),
            endpoint: endpoint.clone(),
            access,
            pairing: control::Pairing::default(),
            gateway: gateway::Manager::load(temp.path())?,
            watchers: Vec::new(),
            refreshes: BTreeMap::new(),
            imports_dir: temp.path().join("imports"),
            updates_tx,
            updates_rx,
            announcements,
            uploads,
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(actor.run(shutdown_rx));
        let mut updates = client.server_streaming(Watch {}, 64).await?;
        assert!(matches!(
            updates
                .recv()
                .await?
                .context("watch closed")?
                .map_err(anyhow::Error::msg)?,
            WatchEvent::GatewayUpdated(_)
        ));
        assert!(matches!(
            updates
                .recv()
                .await?
                .context("watch closed")?
                .map_err(anyhow::Error::msg)?,
            WatchEvent::SnapshotComplete
        ));
        let gateway = client
            .rpc(iroh_share_proto::GetGateway {})
            .await?
            .map_err(anyhow::Error::msg)?;
        assert_eq!(gateway.state, iroh_share_proto::GatewayState::Disabled);
        let config = iroh_share_proto::GatewayConfig {
            listen: "127.0.0.1:0".parse()?,
            ..gateway.config
        };
        client
            .rpc(iroh_share_proto::SetGateway {
                config: config.clone(),
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        assert!(
            matches!(updates.recv().await?.context("watch closed")?.map_err(anyhow::Error::msg)?, WatchEvent::GatewayUpdated(snapshot) if snapshot.config == config)
        );
        assert!(client
            .rpc(iroh_share_proto::SetGateway {
                config: iroh_share_proto::GatewayConfig {
                    listen: "0.0.0.0:8080".parse()?,
                    ..config
                }
            })
            .await?
            .is_err());
        let invitation = client
            .rpc(iroh_share_proto::CreatePairingTicket {})
            .await?
            .map_err(anyhow::Error::msg)?;
        assert_eq!(invitation.addr.id, endpoint.id());
        let completions = client
            .rpc(iroh_share_proto::CompletePath {
                path: temp.path().join("hell"),
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            completions.candidates,
            vec![iroh_share_proto::PathCandidate {
                path: source.clone(),
                kind: iroh_share_proto::PathKind::File,
            }]
        );
        assert!(client
            .rpc(iroh_share_proto::CompletePath {
                path: temp.path().join("missing/")
            })
            .await?
            .is_err());
        let started = client
            .rpc(Share {
                path: source.clone(),
                include_directory_name: false,
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        loop {
            let update = updates
                .recv()
                .await?
                .context("watch closed")?
                .map_err(anyhow::Error::msg)?;
            let WatchEvent::JobUpdated(update) = update else {
                anyhow::bail!("unexpected event");
            };
            assert_eq!(update.id, started.id);
            if let JobState::Importing { progress } = &update.state {
                assert_eq!(progress.bytes_total, 5);
                assert_eq!(progress.files_total, 1);
            }
            if let JobState::Seeding { ticket, .. } = update.state {
                assert_eq!(ticket.addr().id, endpoint.id());
                // Reject a download that has neither providers nor discovery.
                let source = DownloadSource {
                    hash: ticket.hash(),
                    providers: Vec::new(),
                    discovery: DiscoveryMode::Disabled,
                };
                let result = client
                    .rpc(Download {
                        source,
                        target: temp.path().join("download"),
                    })
                    .await?;
                assert_eq!(
                    result.unwrap_err(),
                    "supply at least one provider or enable Mainline discovery"
                );
                break;
            }
        }
        assert_eq!(hashes.borrow().len(), 1);
        assert!(client
            .rpc(iroh_share_proto::Refresh { id: started.id })
            .await?
            .unwrap_err()
            .contains("directory"));
        // A second subscriber receives the full current state, then the boundary.
        let mut snapshot = client.server_streaming(Watch {}, 64).await?;
        let first = snapshot
            .recv()
            .await?
            .context("watch closed")?
            .map_err(anyhow::Error::msg)?;
        assert!(matches!(
            first,
            WatchEvent::JobUpdated(job) if matches!(job.state, JobState::Seeding { .. })
        ));
        assert!(matches!(
            snapshot
                .recv()
                .await?
                .context("watch closed")?
                .map_err(anyhow::Error::msg)?,
            WatchEvent::GatewayUpdated(_)
        ));
        assert!(matches!(
            snapshot
                .recv()
                .await?
                .context("watch closed")?
                .map_err(anyhow::Error::msg)?,
            WatchEvent::SnapshotComplete
        ));
        client
            .rpc(Remove { id: started.id })
            .await?
            .map_err(anyhow::Error::msg)?;
        let removed = updates
            .recv()
            .await?
            .context("watch closed")?
            .map_err(anyhow::Error::msg)?;
        assert!(matches!(removed, WatchEvent::JobRemoved { id } if id == started.id));
        assert!(hashes.borrow().is_empty());
        assert!(client
            .rpc(List {})
            .await?
            .map_err(anyhow::Error::msg)?
            .is_empty());
        assert_eq!(std::fs::read(source)?, b"hello");
        let directory = temp.path().join("website");
        std::fs::create_dir(&directory)?;
        std::fs::write(directory.join("index.html"), "hello")?;
        let shared = client
            .rpc(Share {
                path: directory,
                include_directory_name: false,
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        let mut first_hash = None;
        while let Some(event) = updates.recv().await? {
            if let WatchEvent::JobUpdated(job) = event.map_err(anyhow::Error::msg)? {
                if job.id == shared.id {
                    if let JobState::Seeding { ticket, .. } = job.state {
                        first_hash = Some(ticket.hash());
                        break;
                    }
                }
            }
        }
        assert!(first_hash.is_some());
        assert!(client
            .rpc(iroh_share_proto::Refresh { id: shared.id })
            .await?
            .unwrap_err()
            .contains("name"));
        let name = client
            .rpc(iroh_share_proto::CreateName {
                label: "website".into(),
                target: NameTarget::Job(shared.id),
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        client
            .rpc(iroh_share_proto::Refresh { id: shared.id })
            .await?
            .map_err(anyhow::Error::msg)?;
        let mut importing = false;
        while let Some(event) = updates.recv().await? {
            if let WatchEvent::JobUpdated(job) = event.map_err(anyhow::Error::msg)? {
                if job.id != shared.id {
                    continue;
                }
                if matches!(job.state, JobState::Importing { .. }) {
                    importing = true;
                }
                if let JobState::Seeding { ticket, .. } = job.state {
                    assert!(importing);
                    assert_eq!(Some(ticket.hash()), first_hash);
                    break;
                }
            }
        }
        let names = client
            .rpc(iroh_share_proto::ListNames {})
            .await?
            .map_err(anyhow::Error::msg)?;
        assert_eq!(names[0].key, name.key);
        assert_eq!(names[0].target, NameTarget::Job(shared.id));
        let current = client.rpc(List {}).await?.map_err(anyhow::Error::msg)?;
        let ticket = current
            .iter()
            .find_map(|job| match &job.state {
                JobState::Seeding { ticket, .. } if job.id == shared.id => Some(ticket.clone()),
                _ => None,
            })
            .context("missing seeded directory")?;
        let imported = client
            .rpc(iroh_share_proto::Import {
                source: ticket.clone().try_into()?,
                id: None,
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        let JobKind::Download {
            target: first_target,
            ..
        } = &imported.kind
        else {
            anyhow::bail!("expected imported data")
        };
        assert!(first_target.starts_with(temp.path().join("imports").canonicalize()?));
        loop {
            if let Some(Ok(WatchEvent::JobUpdated(job))) = updates.recv().await? {
                if job.id == imported.id && matches!(job.state, JobState::Seeding { .. }) {
                    break;
                }
            }
        }
        let remote_name = client
            .rpc(iroh_share_proto::CreateName {
                label: "remote-site".into(),
                target: NameTarget::Job(imported.id),
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        std::fs::write(temp.path().join("website/index.html"), "new version")?;
        client
            .rpc(iroh_share_proto::Refresh { id: shared.id })
            .await?
            .map_err(anyhow::Error::msg)?;
        let replacement_ticket = loop {
            if let Some(Ok(WatchEvent::JobUpdated(job))) = updates.recv().await? {
                if job.id == shared.id {
                    if let JobState::Seeding {
                        ticket: new_ticket, ..
                    } = job.state
                    {
                        if new_ticket.hash() != ticket.hash() {
                            break new_ticket;
                        }
                    }
                }
            }
        };
        let updated = client
            .rpc(iroh_share_proto::Import {
                source: replacement_ticket.clone().try_into()?,
                id: Some(imported.id),
            })
            .await?
            .map_err(anyhow::Error::msg)?;
        assert_eq!(updated.id, imported.id);
        let JobKind::Download {
            target: next_target,
            ..
        } = &updated.kind
        else {
            anyhow::bail!("expected updated import")
        };
        assert_ne!(first_target, next_target);
        assert_eq!(std::fs::read(first_target.join("index.html"))?, b"hello");
        loop {
            if let Some(Ok(WatchEvent::JobUpdated(job))) = updates.recv().await? {
                if job.id == imported.id && matches!(job.state, JobState::Seeding { .. }) {
                    break;
                }
            }
        }
        assert_eq!(
            std::fs::read(next_target.join("index.html"))?,
            b"new version"
        );
        let listed_names = client
            .rpc(iroh_share_proto::ListNames {})
            .await?
            .map_err(anyhow::Error::msg)?;
        let updated_name = listed_names
            .iter()
            .find(|name| name.label == "remote-site")
            .unwrap();
        assert_eq!(updated_name.key, remote_name.key);
        assert_eq!(updated_name.target, NameTarget::Job(imported.id));
        let archive = client
            .rpc(iroh_share_proto::ExportNames {})
            .await?
            .map_err(anyhow::Error::msg)?;
        let backup = zip::ZipArchive::new(std::io::Cursor::new(archive))?;
        assert!(backup.file_names().any(|name| name.ends_with(".key")));
        let (restored, _) = names::Names::load(temp.path(), false)?;
        let saved = restored.restored_data();
        assert!(
            matches!(&saved[&imported.id], recovery::SavedData::Download { target, phase: recovery::DownloadPhase::Seeding, .. } if target == next_target)
        );
        assert!(client
            .rpc(iroh_share_proto::Refresh { id: u64::MAX })
            .await?
            .is_err());
        client
            .rpc(iroh_share_proto::Shutdown {})
            .await?
            .map_err(anyhow::Error::msg)?;
        tokio::time::timeout(Duration::from_secs(5), task).await??;
        drop(shutdown_tx);
        control_client.close().await;
        router.shutdown().await?;
        Ok(())
    }
}
