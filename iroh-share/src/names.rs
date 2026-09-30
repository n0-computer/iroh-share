//! Persistent naming keys and signed DNS records, with explicit authenticated backup export.
use crate::recovery::{DownloadPhase, SavedData};
use anyhow::{ensure, Context, Result};
use iroh_share_proto::{
    ImportOutcome, ImportedName, Job, JobError, JobKind, JobState, Name, NameKey, NameState,
    NameTarget, Url,
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
    url: Option<Url>,
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

/// Keys (`<public-key>.key`) and signed records (`<public-key>.pkarr`) in a ZIP.
fn archive<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Result<Vec<u8>> {
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};
    let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    zip.start_file("README.txt", options)?;
    zip.write_all(b"Iroh Share pkarr backup\nFiles are named by their z-base-32 public key.\n<key>.key: 32-byte Ed25519 signing seed, raw binary.\n<key>.pkarr: public key (32 bytes), signature (64 bytes), timestamp (8 bytes, big-endian microseconds), DNS packet. Format: pkarr SignedPacket::as_bytes.\n<key>.pkarr is absent if no record has been created yet.\nThis archive contains private signing keys and is not encrypted.\n")?;
    for entry in entries {
        let key = SigningKey::from_bytes(&entry.secret);
        let public = NameKey(*key.verifying_key().as_bytes());
        zip.start_file(format!("{public}.key"), options)?;
        zip.write_all(&entry.secret)?;
        if let Some(record) = &entry.record {
            zip.start_file(format!("{public}.pkarr"), options)?;
            zip.write_all(&signed_packet(&key, record)?)?;
        }
    }
    Ok(zip.finish()?.into_inner())
}

/// Reads and verifies every key and record in an archive written by [`archive`].
fn read_archive(bytes: &[u8]) -> Result<Vec<ArchivedName>> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("not a ZIP archive")?;
    let mut keys = BTreeMap::new();
    let mut records = BTreeMap::new();
    for index in 0..zip.len() {
        let file = zip.by_index(index)?;
        let name = file.name().to_owned();
        if name == "README.txt" || file.is_dir() {
            continue;
        }
        let public = name
            .rsplit_once('.')
            .and_then(|(stem, _)| z32::decode(stem.as_bytes()).ok())
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .with_context(|| format!("{name}: not named by a public key"))?;
        // Keys are 32 bytes and records at most 1104; anything larger is not ours.
        let mut data = Vec::new();
        file.take(2048).read_to_end(&mut data)?;
        if name.ends_with(".key") {
            let secret = <[u8; 32]>::try_from(data)
                .map_err(|_| anyhow::anyhow!("{name}: expected a 32-byte key"))?;
            ensure!(
                SigningKey::from_bytes(&secret).verifying_key().as_bytes() == &public,
                "{name}: key does not match its file name"
            );
            keys.insert(public, secret);
        } else if name.ends_with(".pkarr") {
            let item = iroh_mainline_endpoint_discovery::decode_signed_packet(&data)
                .with_context(|| format!("{name}: invalid signed packet"))?;
            ensure!(
                item.key() == &public,
                "{name}: record does not match its file name"
            );
            records.insert(public, item);
        } else {
            anyhow::bail!("unexpected file {name}");
        }
    }
    for public in records.keys() {
        ensure!(
            keys.contains_key(public),
            "{}.pkarr has no matching key",
            NameKey(*public)
        );
    }
    ensure!(!keys.is_empty(), "archive contains no names");
    Ok(keys
        .into_iter()
        .map(|(public, secret)| ArchivedName {
            key: NameKey(public),
            secret,
            record: records.remove(&public),
        })
        .collect())
}

struct ArchivedName {
    key: NameKey,
    secret: [u8; 32],
    record: Option<MutableItem>,
}

fn signed_packet(key: &SigningKey, record: &Record) -> Result<Vec<u8>> {
    let item = MutableItem::new(key, &record.packet, record.sequence, None);
    iroh_mainline_endpoint_discovery::encode_signed_packet(&item)
        .context("record cannot be encoded as a Pkarr signed packet")
}

impl Names {
    pub fn export_zip(&self) -> Result<Vec<u8>> {
        archive(self.db.entries.values())
    }

    /// Exports one name's key and record, in the same layout as [`Self::export_zip`].
    pub fn export_name(&self, label: &str) -> Result<Vec<u8>> {
        archive([self.db.entries.get(label).context("unknown name")?])
    }

    /// Adds the names in an exported archive, skipping keys already present.
    ///
    /// The whole archive is validated before anything is saved. A signed
    /// record is kept as-is and republished unchanged until the name is edited.
    pub fn import_zip(
        &mut self,
        bytes: &[u8],
        jobs: &BTreeMap<u64, Job>,
    ) -> Result<Vec<ImportedName>> {
        let imports = read_archive(bytes)?;
        let mut db = self.db.clone();
        let mut outcomes = Vec::new();
        for ArchivedName {
            key,
            secret,
            record: item,
        } in imports
        {
            if db.entries.values().any(|entry| entry.secret == secret) {
                outcomes.push(ImportedName {
                    key,
                    outcome: ImportOutcome::Skipped {
                        reason: "already managed by this daemon".into(),
                    },
                });
                continue;
            }
            let (target, record) = match item {
                Some(item) => {
                    // An unrenderable packet still republishes; only editing needs text.
                    let text = crate::dns_records::text(key, item.value())
                        .unwrap_or_else(|error| format!("; records could not be shown: {error}\n"));
                    let record = Record {
                        url: None,
                        sequence: item.seq(),
                        packet: item.value().to_vec(),
                    };
                    (NameTarget::Records(text), Some(record))
                }
                None => (NameTarget::Records(String::new()), None),
            };
            let base = format!("imported-{}", &key.to_string()[..8]);
            let mut label = base.clone();
            let mut suffix = 2;
            while db.entries.contains_key(&label) {
                label = format!("{base}-{suffix}");
                suffix += 1;
            }
            db.entries.insert(
                label.clone(),
                Entry {
                    secret,
                    target,
                    record,
                    share_path: None,
                },
            );
            outcomes.push(ImportedName {
                key,
                outcome: ImportOutcome::Imported { label },
            });
        }
        if outcomes
            .iter()
            .any(|o| matches!(o.outcome, ImportOutcome::Imported { .. }))
        {
            self.commit(db)?;
            self.refresh(jobs)?;
        }
        Ok(outcomes)
    }

    pub fn load(root: &Path, enabled: bool) -> Result<(Self, watch::Receiver<Vec<Publication>>)> {
        let path = root.join("names.json");
        let mut db: Database = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("invalid names database")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Database::default(),
            Err(error) => return Err(error.into()),
        };
        // Register name-attached paths independently so deleting or retargeting
        // a name does not delete its share.
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
                slot.insert(SavedData::Share {
                    path: path.clone(),
                    include_directory_name: true,
                });
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
    pub fn replace_job(&mut self, id: u64, kind: &JobKind) -> Result<()> {
        let mut db = self.db.clone();
        ensure!(db.data.contains_key(&id), "unknown data");
        db.data.insert(id, SavedData::new(kind));
        db.shares.remove(&id);
        for entry in db.entries.values_mut() {
            if entry.target == NameTarget::Job(id) {
                entry.share_path = None;
            }
        }
        self.commit(db)
    }

    pub fn allocate_job(&mut self, kind: &JobKind) -> Result<u64> {
        let mut db = self.db.clone();
        let id = db.next_job_id;
        db.next_job_id = id.checked_add(1).context("job id exhausted")?;
        db.data.insert(id, SavedData::new(kind));
        if let JobKind::Share { path, .. } = kind {
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
            NameTarget::Records(_) => None,
            NameTarget::Url(url) => {
                validate_url(url)?;
                None
            }
            NameTarget::Job(id) => match &jobs.get(id).context("unknown target job")?.kind {
                JobKind::Share { path, .. } => Some(path.clone()),
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
        if let NameTarget::Records(text) = &entry.target {
            let key = SigningKey::from_bytes(&entry.secret);
            let bytes = crate::dns_records::packet(NameKey(*key.verifying_key().as_bytes()), text)?;
            update_packet(&mut entry, None, bytes)?;
        } else if let Some(url) = desired(&entry.target, jobs) {
            update_record(&mut entry, url)?;
        }
        if let (NameTarget::Job(id), Some(path)) = (&entry.target, &entry.share_path) {
            db.shares.insert(*id, path.clone());
            db.data
                .entry(*id)
                .or_insert_with(|| SavedData::new(&jobs.get(id).expect("linked job exists").kind));
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
                if entry
                    .record
                    .as_ref()
                    .is_none_or(|record| record.url.as_ref() != Some(&url))
                {
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
            let state = match (&entry.record, &entry.target) {
                _ if !self.enabled => Some(NameState::Disabled),
                (None, NameTarget::Records(_)) => Some(NameState::NoRecords),
                (Some(record), _) if available => {
                    if changed.contains(label) || !self.states.contains_key(label) {
                        Some(match &record.url {
                            Some(url) => NameState::Publishing { url: url.clone() },
                            None => NameState::PublishingRecords,
                        })
                    } else {
                        None
                    }
                }
                _ => Some(NameState::WaitingForJob),
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
                NameState::Disabled | NameState::WaitingForJob | NameState::NoRecords
            )
        {
            return None;
        }
        self.states.insert(
            update.label.clone(),
            match update.result {
                Ok(()) => match &record.url {
                    Some(url) => NameState::Published {
                        url: url.clone(),
                        sequence: record.sequence,
                    },
                    None => NameState::PublishedRecords {
                        sequence: record.sequence,
                    },
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
        NameTarget::Records(_) => None,
        NameTarget::Url(url) => Some(url.clone()),
        NameTarget::Job(id) => match &jobs.get(id)?.state {
            JobState::Seeding { ticket, .. } => Some(
                format!(
                    "https://{}.blake3.net/",
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
    let key = SigningKey::from_bytes(&entry.secret);
    let bytes = packet(NameKey(*key.verifying_key().as_bytes()), &url)?;
    update_packet(entry, Some(url), bytes)
}
fn update_packet(entry: &mut Entry, url: Option<Url>, packet: Vec<u8>) -> Result<()> {
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
    fn custom_records_persist_and_invalid_edits_keep_the_record() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut names, _) = Names::load(root.path(), true)?;
        let target = NameTarget::Records("@ 300 IN A 192.0.2.1\n@ 300 IN TXT \"hello\"\n".into());
        let created = names.set("dns".into(), target.clone(), true, &BTreeMap::new())?;
        assert_eq!(created.state, NameState::PublishingRecords);
        let before = names.db.entries["dns"].record.clone().unwrap();
        assert!(names
            .set(
                "dns".into(),
                NameTarget::Records("@ 300 IN A invalid".into()),
                false,
                &BTreeMap::new()
            )
            .is_err());
        assert_eq!(
            names.db.entries["dns"].record.as_ref().unwrap().packet,
            before.packet
        );
        let (mut restored, _) = Names::load(root.path(), true)?;
        restored.refresh(&BTreeMap::new())?;
        let name = restored.list().pop().unwrap();
        assert_eq!(name.target, target);
        assert_eq!(name.key, created.key);
        assert_eq!(
            restored.db.entries["dns"].record.as_ref().unwrap().sequence,
            before.sequence
        );
        assert!(restored
            .publications
            .borrow()
            .iter()
            .any(|p| p.key == created.key));
        Ok(())
    }

    #[test]
    fn export_contains_keys_and_verifiable_records_without_aliases() -> Result<()> {
        use std::io::Read;
        let root = tempfile::tempdir()?;
        let (mut names, _) = Names::load(root.path(), false)?;
        let empty = names.export_zip()?;
        assert_eq!(zip::ZipArchive::new(std::io::Cursor::new(empty))?.len(), 1);
        let name = names.set(
            "private-alias".into(),
            NameTarget::Url("https://example.com/".parse()?),
            true,
            &BTreeMap::new(),
        )?;
        // Include a key that has not produced a record yet.
        names.db.entries.insert(
            "pending-alias".into(),
            Entry {
                secret: [7; 32],
                target: NameTarget::Job(99),
                record: None,
                share_path: None,
            },
        );
        let bytes = names.export_zip()?;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        assert_eq!(zip.len(), 4);
        assert!(zip.file_names().all(|name| !name.contains("alias")));
        let mut secret = Vec::new();
        zip.by_name(&format!("{}.key", name.key))?
            .read_to_end(&mut secret)?;
        let secret: [u8; 32] = secret.try_into().unwrap();
        assert_eq!(
            SigningKey::from_bytes(&secret).verifying_key().as_bytes(),
            &name.key.0
        );
        let mut packet = Vec::new();
        zip.by_name(&format!("{}.pkarr", name.key))?
            .read_to_end(&mut packet)?;
        let public = pkarr::PublicKey::try_from(&name.key.0)?;
        let signed =
            pkarr::SignedPacket::from_relay_payload(&public, &packet[32..].to_vec().into())?;
        assert_eq!(signed.public_key().to_bytes(), name.key.0);
        let record = names.db.entries["private-alias"].record.as_ref().unwrap();
        assert_eq!(&signed.as_bytes()[104..], record.packet.as_slice());
        let pending = NameKey(*SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes());
        assert!(zip.by_name(&format!("{pending}.key")).is_ok());
        assert!(zip.by_name(&format!("{pending}.pkarr")).is_err());

        let single = names.export_name("private-alias")?;
        let mut single = zip::ZipArchive::new(std::io::Cursor::new(single))?;
        assert_eq!(single.len(), 3);
        let mut single_packet = Vec::new();
        single
            .by_name(&format!("{}.pkarr", name.key))?
            .read_to_end(&mut single_packet)?;
        assert_eq!(single_packet, packet);
        assert!(names.export_name("missing").is_err());
        Ok(())
    }

    #[test]
    fn import_restores_keys_and_records_and_skips_known_keys() -> Result<()> {
        let jobs = BTreeMap::new();
        let source = tempfile::tempdir()?;
        let (mut names, _) = Names::load(source.path(), true)?;
        let published = names.set(
            "site".into(),
            NameTarget::Records("@ 300 IN TXT \"hello\"\n".into()),
            true,
            &jobs,
        )?;
        names.db.entries.insert(
            "pending".into(),
            Entry {
                secret: [7; 32],
                target: NameTarget::Job(99),
                record: None,
                share_path: None,
            },
        );
        let archive = names.export_zip()?;
        let original = names.db.entries["site"].record.clone().unwrap();

        let target = tempfile::tempdir()?;
        let (mut restored, _) = Names::load(target.path(), true)?;
        let outcomes = restored.import_zip(&archive, &jobs)?;
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes
            .iter()
            .all(|o| matches!(o.outcome, ImportOutcome::Imported { .. })));
        let by_key = |key: NameKey| {
            let (label, entry) = restored
                .db
                .entries
                .iter()
                .find(|(_, e)| {
                    SigningKey::from_bytes(&e.secret).verifying_key().as_bytes() == &key.0
                })
                .unwrap();
            (label.clone(), entry.clone())
        };
        let (site, entry) = by_key(published.key);
        let record = entry.record.unwrap();
        assert_eq!(record.packet, original.packet);
        assert_eq!(record.sequence, original.sequence);
        assert_eq!(
            entry.target,
            NameTarget::Records("@ 300 IN TXT \"hello\"\n".into())
        );
        let pending = NameKey(*SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes());
        let (pending_label, entry) = by_key(pending);
        assert!(entry.record.is_none());
        assert_eq!(
            restored.get(&pending_label).unwrap().state,
            NameState::NoRecords
        );
        assert!(!restored
            .publications
            .borrow()
            .iter()
            .any(|p| p.key == pending));
        assert!(restored
            .publications
            .borrow()
            .iter()
            .any(|p| p.key == published.key));

        // Restoring again, or into the source, changes nothing.
        let before = restored.list();
        assert!(restored
            .import_zip(&archive, &jobs)?
            .iter()
            .all(|o| matches!(o.outcome, ImportOutcome::Skipped { .. })));
        assert_eq!(restored.list(), before);
        assert!(names
            .import_zip(&archive, &jobs)?
            .iter()
            .all(|o| matches!(o.outcome, ImportOutcome::Skipped { .. })));

        // Editing an imported record moves its sequence forward.
        let edited = restored.set(
            site.clone(),
            NameTarget::Records("@ 300 IN TXT \"bye\"\n".into()),
            false,
            &jobs,
        )?;
        assert_eq!(edited.key, published.key);
        assert!(restored.db.entries[&site].record.as_ref().unwrap().sequence > original.sequence);
        Ok(())
    }

    #[test]
    fn import_accepts_recompressed_archives() -> Result<()> {
        use std::io::Read;
        use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};
        let jobs = BTreeMap::new();
        let source = tempfile::tempdir()?;
        let (mut names, _) = Names::load(source.path(), false)?;
        let name = names.set(
            "site".into(),
            NameTarget::Records("@ 300 IN TXT \"hello\"\n".into()),
            true,
            &jobs,
        )?;
        // Re-zip the export with compression, as desktop archivers do.
        let mut stored = zip::ZipArchive::new(std::io::Cursor::new(names.export_zip()?))?;
        let mut deflated = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for index in 0..stored.len() {
            let mut file = stored.by_index(index)?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            deflated.start_file(
                file.name(),
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )?;
            deflated.write_all(&data)?;
        }
        let archive = deflated.finish()?.into_inner();

        let target = tempfile::tempdir()?;
        let (mut restored, _) = Names::load(target.path(), false)?;
        let outcomes = restored.import_zip(&archive, &jobs)?;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].key, name.key);
        assert!(matches!(
            outcomes[0].outcome,
            ImportOutcome::Imported { .. }
        ));
        Ok(())
    }

    #[test]
    fn import_rejects_mismatched_archives_without_changes() -> Result<()> {
        use zip::{write::SimpleFileOptions, ZipWriter};
        let root = tempfile::tempdir()?;
        let (mut names, _) = Names::load(root.path(), false)?;
        let pending = NameKey(*SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes());
        let write = |files: &[(String, &[u8])]| -> Result<Vec<u8>> {
            let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
            for (name, data) in files {
                zip.start_file(name.as_str(), SimpleFileOptions::default())?;
                zip.write_all(data)?;
            }
            Ok(zip.finish()?.into_inner())
        };
        for archive in [
            write(&[(format!("{pending}.key"), &[8; 32])])?,
            write(&[(format!("{pending}.key"), &[7; 31])])?,
            write(&[(format!("{pending}.pkarr"), &[0; 120])])?,
            write(&[("notes.txt".into(), b"hi")])?,
            write(&[])?,
            b"not a zip".to_vec(),
        ] {
            assert!(names.import_zip(&archive, &BTreeMap::new()).is_err());
        }
        assert!(names.list().is_empty());
        Ok(())
    }

    #[test]
    fn downloads_restore_every_recovery_phase_and_removal() -> Result<()> {
        let root = tempfile::tempdir()?;
        let hash = Hash::new(b"collection");
        let ticket = BlobTicket::new(
            iroh::SecretKey::from_bytes(&[8; 32]).public().into(),
            hash,
            BlobFormat::HashSeq,
        );
        for source in [
            iroh_share_proto::DownloadSource::try_from(ticket)?,
            hash.into(),
        ] {
            let kind = JobKind::Download {
                source: source.clone(),
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
                let JobKind::Download {
                    source: restored, ..
                } = data[&id].kind()
                else {
                    panic!("expected download")
                };
                assert_eq!(restored, source);
            }
            registry.forget_job(id)?;
            assert!(Names::load(root.path(), false)?
                .0
                .restored_data()
                .is_empty());
        }
        Ok(())
    }

    #[test]
    fn restores_saved_ticket_downloads() -> Result<()> {
        let root = tempfile::tempdir()?;
        let ticket = BlobTicket::new(
            iroh::SecretKey::from_bytes(&[8; 32]).public().into(),
            Hash::new(b"collection"),
            BlobFormat::HashSeq,
        );
        let database = serde_json::json!({
            "next_job_id": 1, "entries": {}, "shares": {},
            "data": {"0": {"Download": {"ticket": ticket, "target": root.path().join("target"), "phase": "Exporting"}}}
        });
        std::fs::write(
            root.path().join("names.json"),
            serde_json::to_vec(&database)?,
        )?;
        let (mut registry, _) = Names::load(root.path(), false)?;
        assert_eq!(
            registry.restored_data()[&0].phase(),
            DownloadPhase::Exporting
        );
        let JobKind::Download { source, .. } = registry.restored_data()[&0].kind() else {
            panic!("expected download")
        };
        assert_eq!(source, ticket.try_into()?);
        registry.checkpoint(0, DownloadPhase::Seeding)?;
        assert_eq!(
            Names::load(root.path(), false)?.0.restored_data()[&0].phase(),
            DownloadPhase::Seeding
        );
        Ok(())
    }

    #[test]
    fn unnamed_shares_persist_and_removal_survives_restart() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("directory");
        std::fs::create_dir(&source)?;
        let kind = JobKind::Share {
            path: source.clone(),
            include_directory_name: false,
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
        assert_eq!(
            registry.allocate_job(&JobKind::Share {
                path,
                include_directory_name: false
            })?,
            8
        );
        Ok(())
    }

    #[test]
    fn failed_save_does_not_accept_share_or_consume_id() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (mut registry, _) = Names::load(root.path(), false)?;
        std::fs::create_dir(root.path().join("names.json"))?;
        assert!(registry
            .allocate_job(&JobKind::Share {
                path: root.path().join("share"),
                include_directory_name: false
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
        let id = names.allocate_job(&JobKind::Share {
            path: path.clone(),
            include_directory_name: false,
        })?;
        let mut jobs = BTreeMap::from([(
            id,
            Job {
                id,
                kind: JobKind::Share {
                    path: path.clone(),
                    include_directory_name: false,
                },
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
                active_uploads: 0,
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
                path: root.path().join("another"),
                include_directory_name: false
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
