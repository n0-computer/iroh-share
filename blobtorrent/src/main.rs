mod control;
mod names;
mod recovery;
mod transfer;

use anyhow::{Context, Result};
use blobtorrent_proto::{
    ControlMessage, Job, JobKind, JobState, NameTarget, RpcResult, Url, WatchEvent,
};
use clap::{Parser, Subcommand};
use iroh::{endpoint::presets, protocol::Router, Endpoint};
use iroh_blobs::{store::fs::FsStore, ticket::BlobTicket, BlobFormat, BlobsProtocol, Hash};
use irpc::WithChannels;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[derive(Parser)]
struct Args {
    /// State directory (defaults to the platform's per-user blobtorrent directory).
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
    },
    Share {
        path: PathBuf,
    },
    Download {
        ticket: BlobTicket,
        target: PathBuf,
    },
    List,
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
    Id,
    Endpoint,
    List,
    Allow {
        endpoint: blobtorrent_proto::EndpointId,
    },
    Revoke {
        endpoint: blobtorrent_proto::EndpointId,
    },
}

#[derive(Subcommand)]
enum NameCommand {
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
    watchers: Vec<irpc::channel::mpsc::Sender<RpcResult<WatchEvent>>>,
    updates_tx: mpsc::Sender<Job>,
    updates_rx: mpsc::Receiver<Job>,
    announcements: watch::Sender<std::collections::HashSet<Hash>>,
}

impl Actor {
    async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        loop {
            let message = tokio::select! {
                _ = shutdown.changed() => break,
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
                ControlMessage::Share(message) => {
                    let WithChannels { inner, tx, .. } = message;
                    let result = { self.start(JobKind::Share { path: inner.path }) };
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
                            ticket: inner.ticket,
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
                        if let Some(task) = self.tasks.remove(&inner.id) {
                            task.abort();
                            let _ = task.await;
                        }
                        if let Err(error) = self
                            .store
                            .tags()
                            .delete(format!("blobtorrent/data/{}", inner.id))
                            .await
                        {
                            tracing::warn!(%error, "Cannot remove persistent data tag");
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
                    JobState::Seeding { ticket } => Some(ticket.hash()),
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

    fn start(&mut self, mut kind: JobKind) -> RpcResult<Job> {
        match &mut kind {
            JobKind::Share { path } => {
                *path = path.canonicalize().map_err(|e| e.to_string())?;
                if !path.is_file() && !path.is_dir() {
                    return Err("expected a file or directory".into());
                }
            }
            JobKind::Download { ticket, target } => {
                if ticket.format() != BlobFormat::HashSeq {
                    return Err("expected a collection ticket".into());
                }
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
        self.tasks.insert(
            id,
            tokio::spawn(transfer::run_saved(
                self.store.as_ref().clone(),
                self.endpoint.clone(),
                job.clone(),
                self.updates_tx.clone(),
                phase,
                Some(self.checkpoints_tx.clone()),
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

async fn daemon(state_dir: &Path, no_announce: bool) -> Result<()> {
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
        blobtorrent_proto::client::load_or_create_key(&state_dir.join("control-client.key"))?;
    let access = control::Access::load(state_dir, owner.public())?;
    let server_key = blobtorrent_proto::client::load_or_create_key(&state_dir.join("daemon.key"))?;
    let blob_endpoint = Endpoint::builder(presets::N0)
        .secret_key(server_key)
        .address_lookup(iroh::address_lookup::pkarr::PkarrPublisher::n0_dns())
        .address_lookup(iroh::address_lookup::dns::DnsAddressLookup::n0_dns())
        .bind()
        .await?;
    let (tx, rx) = mpsc::channel(64);
    let router = Router::builder(blob_endpoint.clone())
        .accept(iroh_blobs::ALPN, BlobsProtocol::new(&store, None))
        .accept(
            blobtorrent_proto::CONTROL_ALPN,
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
        Some(n0_mainline::Dht::client()?)
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
        watchers: Vec::new(),
        updates_tx,
        updates_rx,
        announcements,
    };
    for (id, saved) in restored_data {
        actor.spawn_saved(id, saved);
    }
    actor.refresh_names().await;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server_id = actor.endpoint.id();
    let mut actor_task = tokio::spawn(actor.run(shutdown_rx));
    println!("blobtorrent endpoint {server_id}");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
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

async fn client(state_dir: &Path, command: CommandLine) -> Result<()> {
    if matches!(
        command,
        CommandLine::Control {
            command: ControlCommand::Id
        }
    ) {
        println!(
            "{}",
            blobtorrent_proto::client::load_or_create_key(&state_dir.join("control-client.key"))?
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
    let client = blobtorrent_proto::client::ControlClient::connect(state_dir).await?;
    match command {
        CommandLine::Control { command } => match command {
            ControlCommand::Id | ControlCommand::Endpoint => unreachable!(),
            ControlCommand::Allow { endpoint } => client.allow_control(endpoint).await?,
            ControlCommand::Revoke { endpoint } => client.revoke_control(endpoint).await?,
            ControlCommand::List => {
                for endpoint in client.list_control().await? {
                    println!("{endpoint}");
                }
            }
        },
        CommandLine::Share { path } => println!("{:#?}", client.share(path).await?),
        CommandLine::Download { ticket, target } => {
            println!("{:#?}", client.download(ticket, target).await?)
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
                .unwrap_or_else(|_| "warn,blobtorrent=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let state_dir = match args.state_dir {
        Some(path) => path,
        None => blobtorrent_proto::client::default_state_dir()?,
    };
    match args.command {
        CommandLine::Daemon { no_announce } => daemon(&state_dir, no_announce).await,
        command => client(&state_dir, command).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blobtorrent_proto::{ControlProtocol, Download, List, Remove, Share, Watch};

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
            blobtorrent_proto::CONTROL_ALPN,
        );
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&store, None))
            .accept(
                blobtorrent_proto::CONTROL_ALPN,
                control::Control::new(access.clone(), irpc::LocalSender::from(tx)),
            )
            .spawn();
        let (updates_tx, updates_rx) = mpsc::channel(128);
        let (announcements, hashes) = watch::channel(Default::default());
        let (names, _) = names::Names::load(temp.path(), false)?;
        let (_name_updates_tx, name_updates_rx) = mpsc::channel(128);
        let (checkpoints_tx, checkpoints_rx) = mpsc::channel(64);
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
            watchers: Vec::new(),
            updates_tx,
            updates_rx,
            announcements,
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
            WatchEvent::SnapshotComplete
        ));
        let started = client
            .rpc(Share {
                path: source.clone(),
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
            if let JobState::Seeding { ticket } = update.state {
                assert_eq!(ticket.addr().id, endpoint.id());
                // Exercise typed ticket deserialization on an incoming request too.
                let raw = BlobTicket::new(ticket.addr().clone(), ticket.hash(), BlobFormat::Raw);
                let result = client
                    .rpc(Download {
                        ticket: raw,
                        target: temp.path().join("download"),
                    })
                    .await?;
                assert_eq!(result.unwrap_err(), "expected a collection ticket");
                break;
            }
        }
        assert_eq!(hashes.borrow().len(), 1);
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
        shutdown_tx.send(true)?;
        task.await?;
        control_client.close().await;
        router.shutdown().await?;
        Ok(())
    }
}
