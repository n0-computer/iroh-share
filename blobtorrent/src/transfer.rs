use crate::recovery::{checkpoint, Checkpoint, DownloadPhase};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use blobtorrent_proto::{
    DownloadProgress, ExportProgress, ImportProgress, Job, JobError, JobKind, JobState,
};
use iroh::{Endpoint, SecretKey};
use iroh_blobs::{
    api::{
        blobs::{AddPathOptions, AddProgressItem, ExportMode, ExportOptions, ImportMode},
        remote::GetProgressItem,
        Store, TempTag,
    },
    format::collection::Collection,
    ticket::BlobTicket,
    BlobFormat, Hash,
};
use iroh_mainline_endpoint_discovery::{
    infohash_from_blake3, AddrIndex, DiscoveryConfig, Publisher,
};
use n0_future::StreamExt;
use n0_mainline::{Dht, Id};
use tokio::sync::{mpsc, watch};

#[cfg(test)]
pub async fn run(store: Store, endpoint: Endpoint, job: Job, tx: mpsc::Sender<Job>) {
    run_saved(store, endpoint, job, tx, DownloadPhase::Downloading, None).await
}
pub async fn run_saved(
    store: Store,
    endpoint: Endpoint,
    mut job: Job,
    tx: mpsc::Sender<Job>,
    phase: DownloadPhase,
    checkpoints: Option<mpsc::Sender<Checkpoint>>,
) {
    let watched = match &job.kind {
        JobKind::Share { path } => Some(path.clone()),
        _ => None,
    };
    let mut last = None;
    let mut retained = None;
    loop {
        let before = match &watched {
            Some(path) => fingerprint(path).await.ok(),
            None => None,
        };
        match transfer_saved(
            &store,
            &endpoint,
            &mut job,
            &tx,
            phase,
            checkpoints.as_ref(),
        )
        .await
        {
            Ok(tag) => {
                drop(retained.replace(tag));
                last = before;
            }
            Err(error) => {
                job.state = JobState::Failed {
                    error: JobError {
                        message: format!("{error:#}"),
                    },
                };
            }
        }
        if tx.send(job.clone()).await.is_err() {
            return;
        }
        let Some(path) = &watched else {
            std::future::pending::<()>().await;
            return;
        };
        // Require two matching scans before reimporting to debounce ongoing writes.
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let current = fingerprint(path).await.ok();
            if current != last || matches!(job.state, JobState::Failed { .. }) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if fingerprint(path).await.ok() == current {
                    break;
                }
            }
        }
        // Keep the prior collection pinned while its replacement is imported.
    }
}

async fn fingerprint(path: &Path) -> Result<blake3::Hash> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut entries = Vec::new();
        for entry in walkdir::WalkDir::new(path).sort_by_file_name() {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let metadata = entry.metadata()?;
            // Include ctime/inode on Unix so replaced files are noticed even if
            // their lengths and modification timestamps have been preserved.
            #[cfg(unix)]
            let extra = {
                use std::os::unix::fs::MetadataExt;
                (metadata.ino(), metadata.ctime(), metadata.ctime_nsec())
            };
            #[cfg(not(unix))]
            let extra = metadata.created().ok();
            entries.push(format!(
                "{:?} {:?} {} {:?}",
                entry.path(),
                metadata.modified()?,
                metadata.len(),
                extra
            ));
        }
        Ok::<_, anyhow::Error>(blake3::hash(entries.join("\0").as_bytes()))
    })
    .await?
}

async fn report(tx: &mpsc::Sender<Job>, job: &Job) -> Result<()> {
    tx.send(job.clone()).await.context("controller stopped")
}

#[cfg(test)]
async fn transfer(
    store: &Store,
    endpoint: &Endpoint,
    job: &mut Job,
    tx: &mpsc::Sender<Job>,
) -> Result<TempTag> {
    transfer_saved(store, endpoint, job, tx, DownloadPhase::Downloading, None).await
}
async fn transfer_saved(
    store: &Store,
    endpoint: &Endpoint,
    job: &mut Job,
    tx: &mpsc::Sender<Job>,
    phase: DownloadPhase,
    checkpoints: Option<&mpsc::Sender<Checkpoint>>,
) -> Result<TempTag> {
    let tag = match job.kind.clone() {
        JobKind::Share { path } => import(store, &path, job, tx).await?,
        JobKind::Download { ticket, target } => {
            ensure!(
                ticket.format() == BlobFormat::HashSeq,
                "expected a collection ticket"
            );
            let tag = store.tags().temp_tag(ticket.hash_and_format()).await?;
            // Keep completed and partial data rooted across process restarts.
            store
                .tags()
                .set(
                    format!("blobtorrent/data/{}", job.id),
                    ticket.hash_and_format(),
                )
                .await?;
            if phase == DownloadPhase::Seeding {
                ensure!(
                    store
                        .remote()
                        .local(ticket.hash_and_format())
                        .await?
                        .is_complete(),
                    "seed data is no longer complete"
                );
                job.state = JobState::Seeding {
                    ticket: BlobTicket::new(endpoint.addr(), ticket.hash(), BlobFormat::HashSeq),
                };
                return Ok(tag);
            }
            let mut progress = DownloadProgress::default();
            job.state = JobState::Downloading {
                source: ticket.clone(),
                progress: progress.clone(),
            };
            report(tx, job).await?;
            let local = store.remote().local(ticket.hash_and_format()).await?;
            progress.bytes_done = local.local_bytes();
            job.state = JobState::Downloading {
                source: ticket.clone(),
                progress: progress.clone(),
            };
            report(tx, job).await?;
            if !local.is_complete() {
                let conn = endpoint
                    .connect(ticket.addr().clone(), iroh_blobs::ALPN)
                    .await?;
                let base = local.local_bytes();
                let mut stream = store.remote().execute_get(conn, local.missing()).stream();
                while let Some(item) = stream.next().await {
                    match item {
                        GetProgressItem::Progress(offset) => {
                            progress.bytes_done = base + offset;
                            job.state = JobState::Downloading {
                                source: ticket.clone(),
                                progress: progress.clone(),
                            };
                            report(tx, job).await?;
                        }
                        GetProgressItem::Done(_) => break,
                        GetProgressItem::Error(error) => return Err(error.into()),
                    }
                }
            }
            ensure!(
                store
                    .remote()
                    .local(ticket.hash_and_format())
                    .await?
                    .is_complete(),
                "incomplete download"
            );
            let collection = Collection::load(ticket.hash(), store).await?;
            checkpoint(checkpoints, job.id, DownloadPhase::Exporting).await?;
            export(
                store,
                &collection,
                ticket.hash(),
                &target,
                job,
                tx,
                phase == DownloadPhase::Exporting,
            )
            .await?;
            checkpoint(checkpoints, job.id, DownloadPhase::Seeding).await?;
            tag
        }
    };
    job.state = JobState::Seeding {
        ticket: BlobTicket::new(endpoint.addr(), tag.hash(), BlobFormat::HashSeq),
    };
    Ok(tag)
}

async fn import(
    store: &Store,
    path: &Path,
    job: &mut Job,
    tx: &mpsc::Sender<Job>,
) -> Result<TempTag> {
    let root = path.parent().context("cannot share filesystem root")?;
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let path = entry.into_path();
            let relative = path.strip_prefix(root)?;
            let name = relative
                .components()
                .map(|c| c.as_os_str().to_str().context("non-UTF-8 filename"))
                .collect::<Result<Vec<_>>>()?
                .join("/");
            validate_name(&name)?;
            let size = path.metadata()?.len();
            files.push((name, path, size));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut progress = ImportProgress {
        files_total: files.len() as u64,
        bytes_total: files.iter().map(|(_, _, size)| size).sum(),
        ..Default::default()
    };
    job.state = JobState::Importing {
        progress: progress.clone(),
    };
    report(tx, job).await?;
    let mut tags = Vec::new();
    let mut entries = Vec::new();
    for (name, path, size) in files {
        let base = progress.bytes_done;
        let mut stream = store
            .add_path_with_opts(AddPathOptions {
                path,
                mode: ImportMode::TryReference,
                format: BlobFormat::Raw,
            })
            .stream()
            .await;
        let tag = loop {
            match stream.next().await.context("import ended without a tag")? {
                AddProgressItem::Done(tag) => break tag,
                AddProgressItem::Error(error) => return Err(error.into()),
                AddProgressItem::OutboardProgress(offset) => {
                    progress.bytes_done = base + offset;
                    job.state = JobState::Importing {
                        progress: progress.clone(),
                    };
                    report(tx, job).await?;
                }
                _ => {}
            }
        };
        progress.bytes_done = base + size;
        progress.files_done += 1;
        job.state = JobState::Importing {
            progress: progress.clone(),
        };
        report(tx, job).await?;
        entries.push((name, tag.hash()));
        tags.push(tag);
    }
    let collection: Collection = entries.into_iter().collect();
    let root = collection.store(store).await?;
    drop(tags);
    Ok(root)
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "empty collection filename");
    for part in name.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':', '\0']),
            "invalid collection filename: {name}"
        );
        ensure!(
            matches!(
                Path::new(part).components().next(),
                Some(Component::Normal(_))
            ),
            "invalid path component"
        );
    }
    Ok(())
}

// Reject symlink ancestors as well as traversal names before allowing FsStore to
// move payloads into the target. Existing files are never intentionally replaced.
#[cfg(test)]
fn export_path(root: &Path, name: &str) -> Result<PathBuf> {
    export_path_checked(root, name, false)
}
fn export_path_checked(root: &Path, name: &str, resume: bool) -> Result<PathBuf> {
    validate_name(name)?;
    let mut target = root.to_owned();
    let parts: Vec<_> = name.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        target.push(part);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink()
                    && if index + 1 == parts.len() {
                        resume && meta.is_file()
                    } else {
                        meta.is_dir()
                    },
                "export target already exists or is unsafe: {}",
                target.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(target)
}

async fn export(
    store: &Store,
    collection: &Collection,
    root_hash: Hash,
    root: &Path,
    job: &mut Job,
    tx: &mpsc::Sender<Job>,
    resume: bool,
) -> Result<()> {
    let mut targets = HashSet::new();
    let mut exports = Vec::new();
    let mut total = 0;
    for (name, hash) in collection.iter() {
        let target = export_path_checked(root, name, resume)?;
        ensure!(
            targets.insert(target.clone()),
            "duplicate collection filename: {name}"
        );
        let size = store.observe(*hash).await?.size();
        total += size;
        exports.push((name, hash, size));
    }
    // Detect file/directory conflicts before exporting any payload.
    for target in &targets {
        ensure!(
            !target
                .ancestors()
                .skip(1)
                .any(|parent| targets.contains(parent)),
            "conflicting collection paths"
        );
    }
    let mut progress = ExportProgress {
        bytes_total: total,
        files_total: collection.len() as u64,
        ..Default::default()
    };
    job.state = JobState::Exporting {
        root_hash,
        progress: progress.clone(),
    };
    report(tx, job).await?;
    for (name, hash, size) in exports {
        let target = export_path_checked(root, name, resume)?;
        if resume && target.try_exists()? {
            // Published iroh-blobs cannot report the external paths of a blob.
            // Verify an existing output rather than risk copying it onto itself.
            let existing = target.clone();
            let actual = tokio::task::spawn_blocking(move || -> Result<blake3::Hash> {
                let file = std::fs::File::open(existing)?;
                let mut hasher = blake3::Hasher::new();
                hasher.update_reader(file)?;
                Ok(hasher.finalize())
            })
            .await??;
            ensure!(
                actual.as_bytes() == hash.as_bytes(),
                "export target contains different data: {}",
                target.display()
            );
        } else {
            store
                .export_with_opts(ExportOptions {
                    hash: *hash,
                    target,
                    mode: ExportMode::TryReference,
                })
                .await?;
        }
        progress.bytes_done += size;
        progress.files_done += 1;
        job.state = JobState::Exporting {
            root_hash,
            progress: progress.clone(),
        };
        report(tx, job).await?;
    }
    Ok(())
}

/// Retry bootstrap/index discovery, preserving the current set of live jobs.
/// Once created, the publisher owns its background task and publication retries.
/// Public discovery failures must not stop direct ticket transfers.
pub async fn announce(dht: Dht, secret: SecretKey, mut hashes: watch::Receiver<HashSet<Hash>>) {
    loop {
        let result = announce_once(dht.clone(), secret.clone(), &mut hashes).await;
        if hashes.has_changed().is_err() {
            break;
        }
        if let Err(error) = result {
            tracing::warn!(error = %format!("{error:#}"), "Mainline announcement failed; retrying in 30 seconds");
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Report rendezvous advertisements, which are candidates rather than proof of
/// indexer availability. Keep the listing bounded and independent of transfers.
async fn log_rendezvous_indexers(dht: &Dht) {
    let Some(hash) = DiscoveryConfig::default().rendezvous_hash else {
        return;
    };
    let infohash = Id::from(hash);
    tracing::info!(%infohash, "Looking up endpoint indexers via Mainline rendezvous");
    let mut seen = HashSet::new();
    let lookup = async {
        let mut peers = dht.get_peers(infohash).await?;
        while let Some(batch) = peers.next().await {
            for indexer in batch {
                if indexer.port() != 0
                    && !indexer.ip().is_unspecified()
                    && !indexer.ip().is_multicast()
                    && !indexer.ip().is_broadcast()
                    && seen.insert(indexer)
                {
                    tracing::info!(%indexer, "Endpoint indexer advertised at rendezvous");
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    match tokio::time::timeout(Duration::from_secs(30), lookup).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "Endpoint indexer rendezvous lookup failed"),
        Err(_) => tracing::warn!("Endpoint indexer rendezvous lookup timed out after 30 seconds"),
    }
    if seen.is_empty() {
        tracing::warn!(%infohash, "No endpoint indexers found via rendezvous");
    } else {
        tracing::info!(
            count = seen.len(),
            "Endpoint indexer rendezvous listing complete"
        );
    }
}

async fn announce_once(
    dht: Dht,
    secret: SecretKey,
    hashes: &mut watch::Receiver<HashSet<Hash>>,
) -> Result<()> {
    ensure!(dht.bootstrapped().await?, "Mainline bootstrap failed");
    log_rendezvous_indexers(&dht).await;
    let index = AddrIndex::discover(dht.clone()).await?;
    let publisher = Publisher::new(secret, dht, index);
    sync_announcements(&publisher, hashes).await;
    Ok(())
}

// The owner retains the publisher until the daemon drops the hash sender.
async fn sync_announcements(publisher: &Publisher, hashes: &mut watch::Receiver<HashSet<Hash>>) {
    let mut registered = HashSet::new();
    loop {
        let desired: HashSet<_> = hashes
            .borrow_and_update()
            .iter()
            .map(|hash| {
                Id::from(infohash_from_blake3(&blake3::Hash::from_bytes(
                    *hash.as_bytes(),
                )))
            })
            .collect();
        for hash in desired.difference(&registered) {
            publisher.add_infohash(*hash);
        }
        for hash in registered.difference(&desired) {
            publisher.remove_infohash(hash);
        }
        registered = desired;
        if hashes.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn announcements_track_current_hashes_and_stop_on_shutdown() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let dht = Dht::builder().no_bootstrap().port(0).build()?;
            let index = AddrIndex::udp(dht.clone(), "127.0.0.1:9".parse()?).await?;
            let publisher = Publisher::new(SecretKey::from_bytes(&[42; 32]), dht, index);
            let first = Hash::new(b"first");
            let second = Hash::new(b"second");
            let (tx, mut rx) = watch::channel(HashSet::from([first]));
            let task = tokio::spawn({
                let publisher = publisher.clone();
                async move { sync_announcements(&publisher, &mut rx).await }
            });
            for desired in [
                HashSet::from([first]),
                HashSet::from([second]),
                HashSet::new(),
            ] {
                tx.send_replace(desired.clone());
                let expected: HashSet<_> = desired
                    .iter()
                    .map(|hash| {
                        Id::from(infohash_from_blake3(&blake3::Hash::from_bytes(
                            *hash.as_bytes(),
                        )))
                    })
                    .collect();
                while publisher.infohashes().into_iter().collect::<HashSet<_>>() != expected {
                    tokio::task::yield_now().await;
                }
            }
            drop(tx);
            task.await?;
            Ok::<_, anyhow::Error>(())
        })
        .await?
    }

    use iroh::{endpoint::presets, protocol::Router};
    use iroh_blobs::{store::fs::FsStore, BlobsProtocol};

    #[tokio::test]
    async fn export_waits_for_durable_checkpoint() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = FsStore::load(root.path().join("store")).await?;
        let payload = store.add_bytes(b"hello".to_vec()).await?;
        let collection: Collection = vec![("file".to_owned(), payload.hash)]
            .into_iter()
            .collect();
        let tag = collection.store(&store).await?;
        let endpoint = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let target = root.path().join("target");
        std::fs::create_dir(&target)?;
        let mut download = job(JobKind::Download {
            ticket: BlobTicket::new(endpoint.addr(), tag.hash(), BlobFormat::HashSeq),
            target: target.clone(),
        });
        let (tx, _rx) = mpsc::channel(128);
        let (checkpoints, mut events) = mpsc::channel::<Checkpoint>(8);
        let observer = async {
            let event = events.recv().await.context("missing checkpoint")?;
            assert_eq!(event.phase, DownloadPhase::Exporting);
            assert!(!target.join("file").exists());
            event.ack.send(Err("disk full".into())).unwrap();
            Ok::<_, anyhow::Error>(())
        };
        let (result, observed) = tokio::join!(
            transfer_saved(
                &store,
                &endpoint,
                &mut download,
                &tx,
                DownloadPhase::Downloading,
                Some(&checkpoints)
            ),
            observer
        );
        observed?;
        assert!(result.is_err());
        assert!(!target.join("file").exists());
        drop(tag);
        endpoint.close().await;
        store.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn partial_download_resumes_after_store_restart() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), async {
            let root = tempfile::tempdir()?;
            let source = FsStore::load(root.path().join("source")).await?;
            let data: Vec<u8> = (0..1_000_000).map(|i| (i % 251) as u8).collect();
            let payload = source.add_bytes(data.clone()).await?;
            let collection: Collection = vec![("payload".to_owned(), payload.hash)]
                .into_iter()
                .collect();
            let tag = collection.clone().store(&source).await?;
            let provider = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let router = Router::builder(provider.clone())
                .accept(iroh_blobs::ALPN, BlobsProtocol::new(&source, None))
                .spawn();
            let target = root.path().join("target");
            std::fs::create_dir(&target)?;
            let store_path = root.path().join("download");
            let store = FsStore::load(&store_path).await?;
            let metadata = collection.store(&store).await?;
            assert_eq!(metadata.hash(), tag.hash());
            let ranges = bao_tree::ChunkRanges::from(bao_tree::ChunkNum(0)..bao_tree::ChunkNum(16));
            let bao = source
                .export_bao(payload.hash, ranges.clone())
                .bao_to_vec()
                .await?;
            store.import_bao_bytes(payload.hash, ranges, bao).await?;
            drop(metadata);
            store.shutdown().await?;
            let store = FsStore::load(&store_path).await?;
            let local = store.remote().local(tag.hash_and_format()).await?;
            assert!(!local.is_complete());
            assert!(local.local_bytes() >= 16_384);
            let endpoint = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let (tx, mut rx) = mpsc::channel(128);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let mut download = job(JobKind::Download {
                ticket: BlobTicket::new(provider.addr(), tag.hash(), BlobFormat::HashSeq),
                target: target.clone(),
            });
            let complete = transfer_saved(
                &store,
                &endpoint,
                &mut download,
                &tx,
                DownloadPhase::Downloading,
                None,
            )
            .await?;
            assert_eq!(std::fs::read(target.join("payload"))?, data);
            drop(complete);
            drop(tx);
            drain.await?;
            endpoint.close().await;
            store.shutdown().await?;
            drop(tag);
            router.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn exports_resume_at_exact_paths_and_seeding_skips_export() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store_path = root.path().join("store");
        let store = FsStore::load(&store_path).await?;
        let endpoint = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let payload = vec![23u8; 100_000];
        let large = store.add_bytes(payload.clone()).await?;
        let small = store.add_bytes(b"small".to_vec()).await?;
        let collection: Collection = vec![
            ("first".to_owned(), large.hash),
            ("second".to_owned(), large.hash),
            ("tiny".to_owned(), small.hash),
        ]
        .into_iter()
        .collect();
        let tag = collection.store(&store).await?;
        let ticket = BlobTicket::new(
            iroh::SecretKey::from_bytes(&[9; 32]).public().into(),
            tag.hash(),
            BlobFormat::HashSeq,
        );
        let target = root.path().join("target");
        std::fs::create_dir(&target)?;
        // Simulate interruption after exporting just the first collection entry.
        store
            .export_with_opts(ExportOptions {
                hash: large.hash,
                target: target.join("first"),
                mode: ExportMode::TryReference,
            })
            .await?;
        let modified = target.join("first").metadata()?.modified()?;
        drop((large, small, tag));
        store.shutdown().await?;
        let store = FsStore::load(&store_path).await?;
        let (tx, mut rx) = mpsc::channel(128);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let mut download = job(JobKind::Download {
            ticket,
            target: target.clone(),
        });
        let tag = transfer_saved(
            &store,
            &endpoint,
            &mut download,
            &tx,
            DownloadPhase::Exporting,
            None,
        )
        .await?;
        assert_eq!(std::fs::read(target.join("second"))?, payload);
        assert_eq!(target.join("first").metadata()?.modified()?, modified);
        let second_modified = target.join("second").metadata()?.modified()?;
        // Restart in Exporting again: all outputs must be no-ops, including inline data.
        let tiny_modified = target.join("tiny").metadata()?.modified()?;
        let again = transfer_saved(
            &store,
            &endpoint,
            &mut download,
            &tx,
            DownloadPhase::Exporting,
            None,
        )
        .await?;
        assert_eq!(
            target.join("second").metadata()?.modified()?,
            second_modified
        );
        assert_eq!(target.join("tiny").metadata()?.modified()?, tiny_modified);
        drop((tag, again));
        store.shutdown().await?;
        let store = FsStore::load(&store_path).await?;
        let seed = transfer_saved(
            &store,
            &endpoint,
            &mut download,
            &tx,
            DownloadPhase::Seeding,
            None,
        )
        .await?;
        assert_eq!(target.join("tiny").metadata()?.modified()?, tiny_modified);
        assert!(matches!(download.state, JobState::Seeding { .. }));
        // An unrelated existing target must not be adopted or overwritten.
        let other = root.path().join("other");
        std::fs::create_dir(&other)?;
        std::fs::write(other.join("first"), b"keep me")?;
        let mut conflict = job(JobKind::Download {
            ticket: seed_ticket(&download),
            target: other.clone(),
        });
        assert!(transfer_saved(
            &store,
            &endpoint,
            &mut conflict,
            &tx,
            DownloadPhase::Exporting,
            None
        )
        .await
        .is_err());
        assert_eq!(std::fs::read(other.join("first"))?, b"keep me");
        drop(seed);
        drop(tx);
        drain.await?;
        endpoint.close().await;
        store.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn shared_directory_changes_retarget_name() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(20), async {
            let root = tempfile::tempdir()?;
            let source = root.path().join("source");
            std::fs::create_dir(&source)?;
            std::fs::write(source.join("file"), b"before")?;
            let store = FsStore::load(root.path().join("store")).await?;
            let endpoint = Endpoint::builder(presets::Minimal)
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            let share = job(JobKind::Share {
                path: source.clone(),
            });
            let mut jobs = std::collections::BTreeMap::from([(share.id, share.clone())]);
            let (mut names, _) = crate::names::Names::load(root.path(), true)?;
            names.set(
                "test".into(),
                blobtorrent_proto::NameTarget::Job(share.id),
                true,
                &jobs,
            )?;
            let (tx, mut rx) = mpsc::channel(128);
            let task = tokio::spawn(run(store.as_ref().clone(), endpoint.clone(), share, tx));
            let mut urls = Vec::new();
            while let Some(update) = rx.recv().await {
                let seeding = matches!(update.state, JobState::Seeding { .. });
                jobs.insert(update.id, update);
                names.refresh(&jobs)?;
                if seeding {
                    let blobtorrent_proto::NameState::Publishing { url } =
                        names.list()[0].state.clone()
                    else {
                        panic!("missing published URL")
                    };
                    urls.push(url);
                    if urls.len() == 2 {
                        break;
                    }
                    std::fs::write(source.join("file"), b"after change")?;
                }
            }
            assert_eq!(urls.len(), 2);
            assert_ne!(urls[0], urls[1]);
            task.abort();
            let _ = task.await;
            endpoint.close().await;
            store.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    fn seed_ticket(job: &Job) -> BlobTicket {
        match &job.state {
            JobState::Seeding { ticket } => ticket.clone(),
            other => panic!("expected seeding, got {other:?}"),
        }
    }

    fn job(kind: JobKind) -> Job {
        Job {
            id: 0,
            kind,
            state: JobState::Queued,
        }
    }

    #[tokio::test]
    async fn download_exports_references_and_can_seed_after_source_stops() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), roundtrip()).await??;
        Ok(())
    }

    async fn roundtrip() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        std::fs::create_dir(&source)?;
        let payload: Vec<_> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(source.join("large.bin"), &payload)?;
        std::fs::write(source.join("small.txt"), b"hello")?;
        let source = source.canonicalize()?;
        let a = FsStore::load(temp.path().join("a")).await?;
        let b = FsStore::load(temp.path().join("b")).await?;
        let c = FsStore::load(temp.path().join("c")).await?;
        let ep_a = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let ep_b = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let ep_c = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let router_a = Router::builder(ep_a.clone())
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&a, None))
            .spawn();
        let router_b = Router::builder(ep_b.clone())
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&b, None))
            .spawn();
        let (tx, mut rx) = mpsc::channel(128);
        let drain = tokio::spawn(async move {
            let mut snapshots = Vec::new();
            while let Some(job) = rx.recv().await {
                snapshots.push(job);
            }
            snapshots
        });
        let mut share = job(JobKind::Share { path: source });
        let tag_a = transfer(&a, &ep_a, &mut share, &tx).await?;
        let target = temp.path().join("target");
        std::fs::create_dir(&target)?;
        let mut download = job(JobKind::Download {
            ticket: seed_ticket(&share),
            target: target.canonicalize()?,
        });
        let tag_b = transfer(&b, &ep_b, &mut download, &tx).await?;
        assert_eq!(std::fs::read(target.join("source/large.bin"))?, payload);
        assert_eq!(std::fs::read(target.join("source/small.txt"))?, b"hello");
        assert_eq!(seed_ticket(&download).hash(), tag_b.hash());
        let hash = Hash::new(&payload);
        // Large payloads must live only in the target, not also in the store.
        assert!(!temp
            .path()
            .join("b/data")
            .join(format!("{}.data", hash.to_hex()))
            .exists());
        assert!(!temp
            .path()
            .join("a/data")
            .join(format!("{}.data", hash.to_hex()))
            .exists());
        router_a.shutdown().await?;
        drop(tag_a);
        let target_c = temp.path().join("target-c");
        std::fs::create_dir(&target_c)?;
        let mut from_seed = job(JobKind::Download {
            ticket: seed_ticket(&download),
            target: target_c.canonicalize()?,
        });
        let tag_c = transfer(&c, &ep_c, &mut from_seed, &tx).await?;
        assert_eq!(std::fs::read(target_c.join("source/large.bin"))?, payload);
        // Retrying an export must not overwrite the user's existing target.
        assert!(transfer(&b, &ep_b, &mut download, &tx).await.is_err());
        assert_eq!(std::fs::read(target.join("source/large.bin"))?, payload);
        drop((tag_b, tag_c));
        router_b.shutdown().await?;
        ep_c.close().await;
        c.shutdown().await?;
        drop(tx);
        let snapshots = drain.await?;
        assert!(snapshots.iter().any(|job| matches!(&job.state,
            JobState::Exporting { root_hash, progress }
                if *root_hash == seed_ticket(&share).hash()
                    && progress.files_done == 2
                    && progress.bytes_done == payload.len() as u64 + 5
        )));
        assert!(snapshots.iter().any(
            |job| matches!(&job.state, JobState::Downloading { source, .. }
            if source.hash() == seed_ticket(&share).hash())
        ));
        Ok(())
    }

    #[tokio::test]
    async fn failed_transfer_reports_a_failed_state() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = FsStore::load(temp.path().join("store")).await?;
        let endpoint = Endpoint::builder(presets::Minimal)
            .bind_addr("127.0.0.1:0")?
            .bind()
            .await?;
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(run(
            store.as_ref().clone(),
            endpoint.clone(),
            job(JobKind::Share {
                path: temp.path().join("missing"),
            }),
            tx,
        ));
        let failed = rx.recv().await.context("missing failure update")?;
        let JobState::Failed { error } = failed.state else {
            anyhow::bail!("expected failure");
        };
        assert!(!error.message.is_empty());
        task.abort();
        let _ = task.await;
        endpoint.close().await;
        store.shutdown().await?;
        Ok(())
    }

    #[test]
    fn rejects_traversal_existing_files_and_symlink_ancestors() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for name in [
            "../escape",
            "/absolute",
            "a/../b",
            "a//b",
            "a/./b",
            "C:/file",
            "a\\b",
            "",
        ] {
            assert!(export_path(temp.path(), name).is_err(), "{name}");
        }
        std::fs::write(temp.path().join("existing"), b"keep")?;
        assert!(export_path(temp.path(), "existing").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path(), temp.path().join("link"))?;
            assert!(export_path(temp.path(), "link/new").is_err());
        }
        assert!(export_path(temp.path(), "dir/new").is_ok());
        Ok(())
    }
}
