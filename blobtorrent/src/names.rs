//! Persistent naming keys and signed DNS records; never expose secrets over RPC.
use crate::recovery::{DownloadPhase, SavedData};
use anyhow::{ensure, Context, Result};
use blobtorrent_proto::{
    Job, JobError, JobKind, JobState, Name, NameKey, NameState, NameTarget, Url,
};
use n0_mainline::{Dht, MutableItem, SigningKey};
use serde::{Deserialize, Serialize};
use simple_dns::{
    rdata::{RData, HTTPS, NULL, SVCB},
    Packet, ResourceRecord, CLASS,
};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    url: Url,
    sequence: i64,
    packet: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    secret: [u8; 32],
    target: NameTarget,
    record: Option<Record>,
    /// Restore shares attached to names, preserving the association across restarts.
    share_path: Option<PathBuf>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Database {
    next_job_id: u64,
    entries: BTreeMap<String, Entry>,
    /// All shared paths, independent of whether any name points to them.
    #[serde(default)]
    shares: BTreeMap<u64, PathBuf>,
    #[serde(default)]
    data: BTreeMap<u64, SavedData>,
}

pub struct Names {
    path: PathBuf,
    db: Database,
    states: BTreeMap<String, NameState>,
    enabled: bool,
    pub publications: watch::Sender<Vec<Publication>>,
}
#[derive(Clone)]
pub struct Publication {
    pub label: String,
    pub key: NameKey,
    pub item: MutableItem,
}
pub struct Published {
    pub label: String,
    pub key: NameKey,
    pub sequence: i64,
    pub result: Result<(), String>,
}

impl Names {
    pub fn load(root: &Path, enabled: bool) -> Result<(Self, watch::Receiver<Vec<Publication>>)> {
        let path = root.join("names.json");
        let mut db: Database = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("invalid names database")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Database::default(),
            Err(error) => return Err(error.into()),
        };
        // Migrate old name-attached paths before exposing the registry. Once
        // migrated, deleting or retargeting a name must not delete its share.
        let mut migrated = false;
        for entry in db.entries.values() {
            if let (NameTarget::Job(id), Some(path)) = (&entry.target, &entry.share_path) {
                if let std::collections::btree_map::Entry::Vacant(slot) = db.shares.entry(*id) {
                    slot.insert(path.clone());
                    migrated = true;
                }
            }
        }
        for (id, path) in &db.shares {
            if let std::collections::btree_map::Entry::Vacant(slot) = db.data.entry(*id) {
                slot.insert(SavedData::Share { path: path.clone() });
                migrated = true;
            }
        }
        // Reserve IDs referenced by names too, including unavailable downloads.
        let highest = db
            .data
            .keys()
            .copied()
            .chain(db.entries.values().filter_map(|entry| match entry.target {
                NameTarget::Job(id) => Some(id),
                _ => None,
            }))
            .max();
        if let Some(id) = highest {
            let next = id.checked_add(1).context("job id exhausted")?;
            if db.next_job_id < next {
                db.next_job_id = next;
                migrated = true;
            }
        }
        let (publications, rx) = watch::channel(Vec::new());
        let mut names = Self {
            path,
            db,
            states: BTreeMap::new(),
            enabled,
            publications,
        };
        if migrated {
            names.commit(names.db.clone())?;
        }
        Ok((names, rx))
    }
    fn commit(&mut self, db: Database) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&db)?;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", rand::random::<u64>()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)?;
            Ok::<_, std::io::Error>(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.context("saving names database")?;
        self.db = db;
        Ok(())
    }
    pub fn allocate_job(&mut self, kind: &JobKind) -> Result<u64> {
        let mut db = self.db.clone();
        let id = db.next_job_id;
        db.next_job_id = id.checked_add(1).context("job id exhausted")?;
        db.data.insert(id, SavedData::new(kind));
        if let JobKind::Share { path } = kind {
            db.shares.insert(id, path.clone());
        }
        self.commit(db)?;
        Ok(id)
    }
    #[cfg(test)]
    pub fn restored_shares(&self) -> BTreeMap<u64, PathBuf> {
        self.db.shares.clone()
    }

    pub fn restored_data(&self) -> BTreeMap<u64, SavedData> {
        self.db.data.clone()
    }
    pub fn checkpoint(&mut self, id: u64, phase: DownloadPhase) -> Result<()> {
        let mut db = self.db.clone();
        let Some(SavedData::Download { phase: current, .. }) = db.data.get_mut(&id) else {
            anyhow::bail!("unknown download {id}");
        };
        if *current != phase {
            *current = phase;
            self.commit(db)?;
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<Name> {
        self.db
            .entries
            .keys()
            .map(|label| self.get(label).unwrap())
            .collect()
    }
    fn get(&self, label: &str) -> Option<Name> {
        let entry = self.db.entries.get(label)?;
        let key = SigningKey::from_bytes(&entry.secret);
        Some(Name {
            label: label.to_owned(),
            key: NameKey(*key.verifying_key().as_bytes()),
            target: entry.target.clone(),
            state: self.states.get(label).cloned().unwrap_or(if self.enabled {
                NameState::WaitingForJob
            } else {
                NameState::Disabled
            }),
        })
    }
    pub fn set(
        &mut self,
        label: String,
        target: NameTarget,
        create: bool,
        jobs: &BTreeMap<u64, Job>,
    ) -> Result<Name> {
        ensure!(
            !label.is_empty() && label.len() <= 128 && !label.chars().any(char::is_control),
            "name label must contain 1–128 bytes and no control characters"
        );
        ensure!(
            self.db.entries.contains_key(&label) != create,
            if create {
                "name already exists"
            } else {
                "unknown name"
            }
        );
        let share_path = match &target {
            NameTarget::Url(url) => {
                validate_url(url)?;
                None
            }
            NameTarget::Job(id) => match &jobs.get(id).context("unknown target job")?.kind {
                JobKind::Share { path } => Some(path.clone()),
                _ => None,
            },
        };
        let mut db = self.db.clone();
        let secret = db
            .entries
            .get(&label)
            .map(|entry| entry.secret)
            .unwrap_or_else(rand::random);
        let previous = db
            .entries
            .get(&label)
            .and_then(|entry| entry.record.clone());
        let mut entry = Entry {
            secret,
            target,
            record: previous,
            share_path,
        };
        if let Some(url) = desired(&entry.target, jobs) {
            update_record(&mut entry, url)?;
        }
        if let (NameTarget::Job(id), Some(path)) = (&entry.target, &entry.share_path) {
            db.shares.insert(*id, path.clone());
            db.data
                .entry(*id)
                .or_insert_with(|| SavedData::Share { path: path.clone() });
        }
        db.entries.insert(label.clone(), entry);
        self.commit(db)?;
        self.states.remove(&label);
        self.refresh(jobs)?;
        Ok(self.get(&label).unwrap())
    }
    pub fn remove(&mut self, label: &str, jobs: &BTreeMap<u64, Job>) -> Result<()> {
        let mut db = self.db.clone();
        ensure!(db.entries.remove(label).is_some(), "unknown name");
        self.commit(db)?;
        self.states.remove(label);
        self.refresh(jobs)?;
        Ok(())
    }
    pub fn forget_job(&mut self, id: u64) -> Result<()> {
        let mut db = self.db.clone();
        db.shares.remove(&id);
        db.data.remove(&id);
        for entry in db.entries.values_mut() {
            if entry.target == NameTarget::Job(id) {
                entry.share_path = None;
            }
        }
        self.commit(db)
    }
    pub fn refresh(&mut self, jobs: &BTreeMap<u64, Job>) -> Result<Vec<Name>> {
        let before = self.list();
        let mut db = self.db.clone();
        let mut changed = Vec::new();
        let mut dirty = false;
        for (label, entry) in &mut db.entries {
            if let Some(url) = desired(&entry.target, jobs) {
                if entry.record.as_ref().is_none_or(|record| record.url != url) {
                    update_record(entry, url)?;
                    dirty = true;
                    changed.push(label.clone());
                }
            }
        }
        if dirty {
            self.commit(db)?;
        }
        let mut publications = Vec::new();
        for (label, entry) in &self.db.entries {
            let available = match &entry.target {
                NameTarget::Job(id) => jobs.contains_key(id),
                _ => true,
            };
            let state = if !self.enabled {
                Some(NameState::Disabled)
            } else if !available || entry.record.is_none() {
                Some(NameState::WaitingForJob)
            } else if changed.contains(label) || !self.states.contains_key(label) {
                Some(NameState::Publishing {
                    url: entry.record.as_ref().unwrap().url.clone(),
                })
            } else {
                None
            };
            if let Some(state) = state {
                self.states.insert(label.clone(), state);
            }
            if available && self.enabled {
                if let Some(record) = &entry.record {
                    let key = SigningKey::from_bytes(&entry.secret);
                    publications.push(Publication {
                        label: label.clone(),
                        key: NameKey(*key.verifying_key().as_bytes()),
                        item: MutableItem::new(&key, &record.packet, record.sequence, None),
                    });
                }
            }
        }
        // Avoid cancelling/restarting publication on unrelated progress events.
        self.publications.send_if_modified(|current| {
            let same = current.len() == publications.len()
                && current.iter().zip(&publications).all(|(a, b)| {
                    a.label == b.label && a.key == b.key && a.item.seq() == b.item.seq()
                });
            if same {
                false
            } else {
                *current = publications;
                true
            }
        });
        Ok(self
            .list()
            .into_iter()
            .filter(|name| !before.contains(name))
            .collect())
    }
    pub fn published(&mut self, update: Published) -> Option<Name> {
        let entry = self.db.entries.get(&update.label)?;
        let current = self.get(&update.label)?;
        let record = entry.record.as_ref()?;
        if current.key != update.key
            || record.sequence != update.sequence
            || matches!(
                current.state,
                NameState::Disabled | NameState::WaitingForJob
            )
        {
            return None;
        }
        self.states.insert(
            update.label.clone(),
            match update.result {
                Ok(()) => NameState::Published {
                    url: record.url.clone(),
                    sequence: record.sequence,
                },
                Err(message) => NameState::Failed {
                    error: JobError { message },
                },
            },
        );
        self.get(&update.label)
    }
}
fn desired(target: &NameTarget, jobs: &BTreeMap<u64, Job>) -> Option<Url> {
    match target {
        NameTarget::Url(url) => Some(url.clone()),
        NameTarget::Job(id) => match &jobs.get(id)?.state {
            JobState::Seeding { ticket } => Some(
                format!(
                    "https://{}.blake3.link/",
                    z32::encode(ticket.hash().as_bytes())
                )
                .parse()
                .unwrap(),
            ),
            _ => None,
        },
    }
}
fn update_record(entry: &mut Entry, url: Url) -> Result<()> {
    let now: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_micros()
        .try_into()?;
    let sequence = now.max(
        entry
            .record
            .as_ref()
            .map(|r| r.sequence.saturating_add(1))
            .unwrap_or(0),
    );
    let key = SigningKey::from_bytes(&entry.secret);
    let packet = packet(NameKey(*key.verifying_key().as_bytes()), &url)?;
    entry.record = Some(Record {
        url,
        sequence,
        packet,
    });
    Ok(())
}
fn validate_url(url: &Url) -> Result<()> {
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "target must be an http(s) URL without credentials"
    );
    Ok(())
}
/// URI RR (RFC 7553, type 256): priority, weight, unquoted target octets.
fn packet(key: NameKey, url: &Url) -> Result<Vec<u8>> {
    validate_url(url)?;
    let owner = key.to_string();
    let mut packet = Packet::new_reply(0);
    let uri_owner = format!("_https._tcp.{owner}");
    let mut uri = vec![0, 0, 0, 0];
    uri.extend_from_slice(url.as_str().as_bytes());
    packet.answers.push(ResourceRecord::new(
        uri_owner.as_str().try_into()?,
        CLASS::IN,
        300,
        RData::NULL(256, NULL::new(&uri)?),
    ));
    // Preserve compatibility with HTTPS-only resolvers for origin-only URLs.
    if url.scheme() == "https"
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
    {
        if let Some(host) = url.domain() {
            let mut svcb = SVCB::new(if url.port().is_some() { 1 } else { 0 }, host.try_into()?);
            if let Some(port) = url.port() {
                svcb.set_port(port);
            }
            packet.answers.push(ResourceRecord::new(
                owner.as_str().try_into()?,
                CLASS::IN,
                300,
                RData::HTTPS(HTTPS(svcb)),
            ));
        }
    }
    let packet = packet.build_bytes_vec_compressed()?;
    ensure!(
        packet.len() <= 1000,
        "target URL exceeds the BEP44 packet size limit"
    );
    Ok(packet)
}

pub async fn publish(
    dht: Dht,
    mut rx: watch::Receiver<Vec<Publication>>,
    tx: mpsc::Sender<Published>,
) {
    loop {
        let entries = rx.borrow_and_update().clone();
        let round = async {
            let mut failed = false;
            for entry in entries {
                let result = match tokio::time::timeout(
                    Duration::from_secs(60),
                    dht.put_mutable(entry.item.clone(), None),
                )
                .await
                {
                    Ok(result) => result.map(|_| ()).map_err(|error| error.to_string()),
                    Err(_) => Err("pkarr publication timed out".into()),
                };
                failed |= result.is_err();
                if let Err(error) = &result {
                    tracing::warn!(name = %entry.label, %error, "Pkarr publication failed");
                } else {
                    tracing::info!(name = %entry.label, url = %entry.key.url(), "Published pkarr name");
                }
                if tx
                    .send(Published {
                        label: entry.label,
                        key: entry.key,
                        sequence: entry.item.seq(),
                        result,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(if failed { 30 } else { 600 })).await;
        };
        tokio::select! { _ = tx.closed() => break, _ = round => {}, result = rx.changed() => if result.is_err() { break; } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_blobs::{ticket::BlobTicket, BlobFormat, Hash};

    #[test]
    fn downloads_restore_every_recovery_phase_and_removal() -> Result<()> {
        let root = tempfile::tempdir()?;
        let ticket = BlobTicket::new(
            iroh::SecretKey::from_bytes(&[8; 32]).public().into(),
            Hash::new(b"collection"),
            BlobFormat::HashSeq,
        );
        let kind = JobKind::Download {
            ticket,
            target: root.path().join("target"),
        };
        let (mut registry, _) = Names::load(root.path(), false)?;
        let id = registry.allocate_job(&kind)?;
        for phase in [
            DownloadPhase::Downloading,
            DownloadPhase::Exporting,
            DownloadPhase::Seeding,
        ] {
            registry.checkpoint(id, phase)?;
            let (loaded, _) = Names::load(root.path(), false)?;
            let data = loaded.restored_data();
            assert_eq!(data[&id].phase(), phase);
            assert!(matches!(data[&id].kind(), JobKind::Download { .. }));
        }
        registry.forget_job(id)?;
        assert!(Names::load(root.path(), false)?
            .0
            .restored_data()
            .is_empty());
        Ok(())
    }

    #[test]
    fn unnamed_shares_persist_and_removal_survives_restart() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("directory");
        std::fs::create_dir(&source)?;
        let kind = JobKind::Share {
            path: source.clone(),
        };
        let (mut registry, _) = Names::load(root.path(), false)?;
        let id = registry.allocate_job(&kind)?;
        drop(registry);
        // A missing path must remain registered so a disconnected drive can return.
        std::fs::remove_dir(&source)?;
        let (mut registry, _) = Names::load(root.path(), false)?;
        assert_eq!(registry.restored_shares().get(&id), Some(&source));
        let next = registry.allocate_job(&kind)?;
        assert!(next > id);
        registry.forget_job(id)?;
        let (registry, _) = Names::load(root.path(), false)?;
        assert!(!registry.restored_shares().contains_key(&id));
        assert!(registry.restored_shares().contains_key(&next));
        Ok(())
    }

    #[test]
    fn migrates_legacy_named_shares_without_linking_their_lifetime() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("shared");
        let legacy = serde_json::json!({
            "next_job_id": 8,
            "entries": {
                "site": {
                    "secret": vec![3; 32],
                    "target": NameTarget::Job(7),
                    "record": null,
                    "share_path": path,
                }
            }
        });
        std::fs::write(root.path().join("names.json"), serde_json::to_vec(&legacy)?)?;
        let (mut registry, _) = Names::load(root.path(), false)?;
        assert_eq!(registry.restored_shares().get(&7), Some(&path));
        registry.remove("site", &BTreeMap::new())?;
        let (mut registry, _) = Names::load(root.path(), false)?;
        assert_eq!(registry.restored_shares().get(&7), Some(&path));
        assert_eq!(registry.allocate_job(&JobKind::Share { path })?, 8);
        Ok(())
    }

    #[test]
    fn failed_save_does_not_accept_share_or_consume_id() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut registry, _) = Names::load(root.path(), false)?;
        std::fs::create_dir(root.path().join("names.json"))?;
        assert!(registry
            .allocate_job(&JobKind::Share {
                path: root.path().join("share")
            })
            .is_err());
        assert!(registry.restored_shares().is_empty());
        assert_eq!(registry.db.next_job_id, 0);
        assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn publisher_roundtrip_on_local_dht() -> Result<()> {
        use n0_future::StreamExt;
        tokio::time::timeout(Duration::from_secs(30), async {
            let network = n0_mainline::Testnet::new(3).await?;
            let writer = Dht::builder()
                .bootstrap(&network.bootstrap)
                .port(0)
                .build()?;
            let reader = Dht::builder()
                .bootstrap(&network.bootstrap)
                .port(0)
                .build()?;
            let root = tempfile::tempdir()?;
            let (mut names, publications) = Names::load(root.path(), true)?;
            let name = names.set(
                "site".into(),
                NameTarget::Url("https://example.com/path?q=1".parse()?),
                true,
                &BTreeMap::new(),
            )?;
            let (tx, mut rx) = mpsc::channel(8);
            let task = tokio::spawn(publish(writer, publications, tx));
            let update = rx.recv().await.context("publisher stopped")?;
            assert!(update.result.is_ok(), "{:?}", update.result);
            names.published(update).context("publication ignored")?;
            let mut items = reader.get_mutable(&name.key.0, None, None).await?;
            let item = items.next().await.context("missing signed record")?;
            assert_eq!(
                item.value(),
                names.db.entries["site"].record.as_ref().unwrap().packet
            );
            drop(rx);
            task.await?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    #[test]
    fn persistent_keys_follow_jobs_and_ignore_stale_publication() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut names, rx) = Names::load(root.path(), true)?;
        let path = root.path().join("shared");
        let id = names.allocate_job(&JobKind::Share { path: path.clone() })?;
        let mut jobs = BTreeMap::from([(
            id,
            Job {
                id,
                kind: JobKind::Share { path: path.clone() },
                state: JobState::Queued,
            },
        )]);
        let name = names.set("site".into(), NameTarget::Job(id), true, &jobs)?;
        assert_eq!(name.state, NameState::WaitingForJob);
        assert!(rx.borrow().is_empty());
        let endpoint = iroh::SecretKey::from_bytes(&[7; 32]).public();
        for content in [b"first".as_slice(), b"second".as_slice()] {
            let hash = Hash::new(content);
            jobs.get_mut(&id).unwrap().state = JobState::Seeding {
                ticket: BlobTicket::new(endpoint.into(), hash, BlobFormat::HashSeq),
            };
            assert_eq!(names.refresh(&jobs)?.len(), 1);
            assert_eq!(names.get("site").unwrap().key, name.key);
            assert!(names.refresh(&jobs)?.is_empty());
        }
        let publication = rx.borrow()[0].clone();
        let (mut restored, _) = Names::load(root.path(), true)?;
        assert_eq!(restored.get("site").unwrap().key, name.key);
        assert_eq!(restored.restored_shares().get(&id), Some(&path));
        assert!(
            restored.allocate_job(&JobKind::Share {
                path: root.path().join("another")
            })? > id
        );
        restored.refresh(&jobs)?;
        assert_eq!(
            restored.db.entries["site"]
                .record
                .as_ref()
                .unwrap()
                .sequence,
            publication.item.seq()
        );
        let target: Url = "https://example.com/some/path?q=1#fragment".parse()?;
        restored.set("site".into(), NameTarget::Url(target.clone()), false, &jobs)?;
        assert_eq!(restored.get("site").unwrap().key, name.key);
        assert!(
            restored.db.entries["site"]
                .record
                .as_ref()
                .unwrap()
                .sequence
                > publication.item.seq()
        );
        assert!(restored
            .published(Published {
                label: "site".into(),
                key: name.key,
                sequence: publication.item.seq(),
                result: Ok(())
            })
            .is_none());
        let bytes = &restored.db.entries["site"].record.as_ref().unwrap().packet;
        let packet = Packet::parse(bytes)?;
        let RData::NULL(256, data) = &packet.answers[0].rdata else {
            panic!("URI record missing")
        };
        assert_eq!(&data.get_data()[4..], target.as_str().as_bytes());
        restored.set("site".into(), NameTarget::Job(id), false, &jobs)?;
        restored.forget_job(id)?;
        jobs.clear();
        restored.refresh(&jobs)?;
        assert_eq!(
            restored.get("site").unwrap().state,
            NameState::WaitingForJob
        );
        assert!(!Names::load(root.path(), true)?
            .0
            .restored_shares()
            .contains_key(&id));
        restored.remove("site", &jobs)?;
        assert!(Names::load(root.path(), true)?.0.list().is_empty());
        Ok(())
    }

    #[test]
    fn rejects_invalid_targets_without_persisting() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut names, _) = Names::load(root.path(), true)?;
        for url in [
            "file:///tmp/data",
            "https://user:password@example.com/",
            &format!("https://example.com/{}", "x".repeat(1100)),
        ] {
            assert!(names
                .set(
                    "bad".into(),
                    NameTarget::Url(url.parse()?),
                    true,
                    &BTreeMap::new()
                )
                .is_err());
            assert!(names.list().is_empty());
        }
        Ok(())
    }
}
