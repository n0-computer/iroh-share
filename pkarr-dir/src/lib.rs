//! A directory of pkarr records.
//!
//! Each name is a pair of files with the same stem: `<name>.pkarr`, the signed
//! record, and optionally `<name>.key`, the 32-byte Ed25519 signing seed. Only
//! the machine that edits a name needs its key; republishing needs just the
//! signed record.
//!
//! A `.pkarr` file uses the pkarr `SignedPacket::as_bytes` layout: the public
//! key (32 bytes), the signature (64 bytes), the timestamp in microseconds
//! (8 bytes, big-endian), then the DNS packet. The timestamp is also the
//! BEP 44 sequence number, so a newer record always wins.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, ensure, Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use n0_mainline::{MutableItem, SigningKey};

pub mod records;

/// File extension of signed records.
pub const RECORD_EXTENSION: &str = "pkarr";
/// File extension of signing keys.
pub const KEY_EXTENSION: &str = "key";

/// Encodes a signed record in the `.pkarr` file layout.
pub fn encode(item: &MutableItem) -> Result<Vec<u8>> {
    ensure!(item.salt().is_none(), "pkarr records are never salted");
    let seq = u64::try_from(item.seq()).context("negative sequence number")?;
    let mut bytes = Vec::with_capacity(104 + item.value().len());
    bytes.extend_from_slice(item.key());
    bytes.extend_from_slice(item.signature());
    bytes.extend_from_slice(&seq.to_be_bytes());
    bytes.extend_from_slice(item.value());
    Ok(bytes)
}

/// Decodes a `.pkarr` file and verifies its signature.
pub fn decode(bytes: &[u8]) -> Result<MutableItem> {
    let (key, rest) = bytes
        .split_first_chunk::<32>()
        .context("record is too short")?;
    let (signature, rest) = rest
        .split_first_chunk::<64>()
        .context("record is too short")?;
    let (seq, value) = rest
        .split_first_chunk::<8>()
        .context("record is too short")?;
    let seq = i64::try_from(u64::from_be_bytes(*seq)).context("sequence number out of range")?;
    ensure!(value.len() <= 1000, "DNS packet exceeds 1000 bytes");
    VerifyingKey::from_bytes(key)
        .context("invalid public key")?
        .verify_strict(&signable(seq, value), &Signature::from_bytes(signature))
        .context("invalid signature")?;
    Ok(MutableItem::new_signed_unchecked(
        *key, *signature, value, seq, None,
    ))
}

/// Signs `packet` with `key`, with a sequence number newer than `previous`.
///
/// The sequence number is the current time in microseconds, as pkarr expects,
/// or one more than `previous` if the clock is behind it.
pub fn sign(key: &SigningKey, packet: &[u8], previous: Option<i64>) -> Result<MutableItem> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros();
    let now = i64::try_from(now).context("clock out of range")?;
    let seq = previous.map_or(now, |previous| now.max(previous.saturating_add(1)));
    Ok(MutableItem::new(key, packet, seq, None))
}

/// The bytes BEP 44 signs for an unsalted item.
fn signable(seq: i64, value: &[u8]) -> Vec<u8> {
    let mut bytes = format!("3:seqi{seq}e1:v{}:", value.len()).into_bytes();
    bytes.extend_from_slice(value);
    bytes
}

/// The files of one name in the directory.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub record: Option<PathBuf>,
    pub key: Option<PathBuf>,
}

/// Lists the names in `dir`, sorted, with whichever files each has.
pub fn scan(dir: &Path) -> Result<Vec<Entry>> {
    let mut entries = BTreeMap::<String, Entry>::new();
    for file in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = file?.path();
        let (Some(stem), Some(extension)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        if stem.starts_with('.') || !path.is_file() {
            continue;
        }
        let entry = entries.entry(stem.to_owned()).or_insert_with(|| Entry {
            name: stem.to_owned(),
            record: None,
            key: None,
        });
        match extension {
            RECORD_EXTENSION => entry.record = Some(path),
            KEY_EXTENSION => entry.key = Some(path),
            _ => {}
        }
    }
    Ok(entries
        .into_values()
        .filter(|entry| entry.record.is_some() || entry.key.is_some())
        .collect())
}

/// Finds `name` in `dir`.
pub fn entry(dir: &Path, name: &str) -> Result<Entry> {
    ensure!(
        !name.is_empty() && !name.contains(['/', '\\']) && !name.starts_with('.'),
        "invalid name {name:?}"
    );
    let record = dir.join(format!("{name}.{RECORD_EXTENSION}"));
    let key = dir.join(format!("{name}.{KEY_EXTENSION}"));
    Ok(Entry {
        name: name.to_owned(),
        record: record.is_file().then_some(record),
        key: key.is_file().then_some(key),
    })
}

/// Reads and verifies a `.pkarr` file.
pub fn read_record(path: &Path) -> Result<MutableItem> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    decode(&bytes).with_context(|| format!("{} is not a valid record", path.display()))
}

/// Writes a `.pkarr` file, replacing any previous version atomically.
pub fn write_record(path: &Path, item: &MutableItem) -> Result<()> {
    let bytes = encode(item)?;
    let tmp = path.with_extension(format!("{RECORD_EXTENSION}.tmp"));
    fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

/// Reads a signing key, refusing one that other users can read.
pub fn read_key(path: &Path) -> Result<SigningKey> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            bail!(
                "{} is readable by other users; run `chmod 600 {}`",
                path.display(),
                path.display()
            );
        }
    }
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let seed = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| anyhow::anyhow!("{} must contain exactly 32 bytes", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Checks that a name's key and record belong together.
pub fn check_pair(key: &SigningKey, record: &MutableItem) -> Result<()> {
    ensure!(
        key.verifying_key().as_bytes() == record.key(),
        "the key does not match the record"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_records_round_trip_and_reject_tampering() -> Result<()> {
        let key = SigningKey::from_bytes(&[3; 32]);
        let packet = records::packet(key.verifying_key().as_bytes(), "@ 300 IN TXT \"hi\"\n")?;
        let item = sign(&key, &packet, None)?;
        let bytes = encode(&item)?;
        let decoded = decode(&bytes)?;
        assert_eq!(decoded.value(), packet.as_slice());
        assert_eq!(decoded.seq(), item.seq());
        let mut tampered = bytes.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decode(&tampered).is_err());
        // A newer signature always gets a higher sequence number.
        let later = sign(&key, &packet, Some(i64::MAX - 1))?;
        assert_eq!(later.seq(), i64::MAX);
        Ok(())
    }

    #[test]
    fn scans_pairs_and_rejects_bad_names() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join("a.pkarr"), b"")?;
        fs::write(dir.path().join("a.key"), b"")?;
        fs::write(dir.path().join("b.key"), b"")?;
        fs::write(dir.path().join("notes.txt"), b"")?;
        let entries = scan(dir.path())?;
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(entries[0].record.is_some() && entries[0].key.is_some());
        assert!(entries[1].record.is_none() && entries[1].key.is_some());
        assert!(entry(dir.path(), "../a").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn refuses_keys_readable_by_others() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("a.key");
        fs::write(&path, [1; 32])?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        assert!(read_key(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        read_key(&path)?;
        Ok(())
    }
}
