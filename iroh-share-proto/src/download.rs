use std::{fmt, str::FromStr};

use iroh_blobs::{BlobFormat, HashAndFormat};
use serde::{Deserialize, Serialize};

use crate::{BlobTicket, Hash, Url};

/// Whether the daemon may find providers beyond the supplied addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryMode {
    /// Only contact the supplied providers.
    Disabled,
    /// Try supplied providers first, then discover more if needed.
    Mainline,
}

/// A collection root and optional provider hints, parsed by the client.
///
/// Parses blake3.net root URLs, z32/hex hashes, and collection blob tickets.
/// URLs and hashes enable discovery; tickets restrict downloads to their provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadSource {
    pub hash: Hash,
    /// Empty when no provider addresses are supplied.
    pub providers: Vec<iroh::EndpointAddr>,
    pub discovery: DiscoveryMode,
}

impl DownloadSource {
    pub fn hash(&self) -> Hash {
        self.hash
    }

    pub fn hash_and_format(&self) -> HashAndFormat {
        HashAndFormat::new(self.hash, BlobFormat::HashSeq)
    }

    pub fn validate(&self) -> Result<(), ParseDownloadSourceError> {
        if self.providers.is_empty() && self.discovery == DiscoveryMode::Disabled {
            return Err(ParseDownloadSourceError(
                "supply at least one provider or enable Mainline discovery",
            ));
        }
        Ok(())
    }
}

impl TryFrom<BlobTicket> for DownloadSource {
    type Error = ParseDownloadSourceError;

    fn try_from(ticket: BlobTicket) -> Result<Self, Self::Error> {
        if ticket.format() != BlobFormat::HashSeq {
            return Err(ParseDownloadSourceError("expected a collection ticket"));
        }
        Ok(Self {
            hash: ticket.hash(),
            providers: vec![ticket.addr().clone()],
            discovery: DiscoveryMode::Disabled,
        })
    }
}

impl From<Hash> for DownloadSource {
    fn from(hash: Hash) -> Self {
        Self {
            hash,
            providers: Vec::new(),
            discovery: DiscoveryMode::Mainline,
        }
    }
}

impl fmt::Display for DownloadSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "https://{}.blake3.net/",
            z32::encode(self.hash.as_bytes())
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDownloadSourceError(&'static str);

impl fmt::Display for ParseDownloadSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ParseDownloadSourceError {}

fn z32_hash(value: &str) -> Option<Hash> {
    if value.len() != 52 {
        return None;
    }
    let bytes: [u8; 32] = z32::decode(value.as_bytes()).ok()?.try_into().ok()?;
    (z32::encode(&bytes) == value).then(|| Hash::from_bytes(bytes))
}

impl FromStr for DownloadSource {
    type Err = ParseDownloadSourceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.contains("://") {
            let url =
                Url::parse(value).map_err(|_| ParseDownloadSourceError("invalid content URL"))?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port().is_some()
            {
                return Err(ParseDownloadSourceError("expected an http(s) blake3.net collection URL without credentials or a custom port"));
            }
            if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
                return Err(ParseDownloadSourceError(
                    "use the collection root URL without a subpath, query, or fragment",
                ));
            }
            let label = url
                .host_str()
                .and_then(|host| host.strip_suffix(".blake3.net"));
            return label
                .and_then(z32_hash)
                .map(Self::from)
                .ok_or(ParseDownloadSourceError(
                    "expected https://<z32-hash>.blake3.net/",
                ));
        }
        if value.len() == 64 {
            if let Ok(hash) = value.to_ascii_lowercase().parse::<Hash>() {
                return Ok(Self::from(hash));
            }
        }
        if let Some(hash) = z32_hash(value) {
            return Ok(Self::from(hash));
        }
        if let Ok(ticket) = value.parse::<BlobTicket>() {
            return Self::try_from(ticket);
        }
        Err(ParseDownloadSourceError("expected a blake3.net collection URL, a 52-character z32 or 64-character hex hash, or a collection ticket"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls_hashes_and_collection_tickets() {
        let hash = Hash::new(b"collection");
        let z32 = z32::encode(hash.as_bytes());
        for input in [
            hash.to_hex().to_string(),
            z32.clone(),
            format!("https://{z32}.blake3.net/"),
            format!(" http://{z32}.blake3.net \n"),
        ] {
            assert_eq!(
                input.parse::<DownloadSource>().unwrap(),
                DownloadSource::from(hash)
            );
        }
        let addr = iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[1; 32]).public())
            .with_ip_addr("127.0.0.1:1234".parse().unwrap())
            .with_relay_url("https://relay.example.com".parse().unwrap());
        let ticket = BlobTicket::new(addr.clone(), hash, BlobFormat::HashSeq);
        let source = ticket.to_string().parse::<DownloadSource>().unwrap();
        assert_eq!(source.hash, hash);
        assert_eq!(source.providers, vec![addr]);
        assert_eq!(source.discovery, DiscoveryMode::Disabled);
        let encoded = postcard::to_allocvec(&source).unwrap();
        assert_eq!(
            postcard::from_bytes::<DownloadSource>(&encoded).unwrap(),
            source
        );
    }

    #[test]
    fn rejects_ambiguous_or_non_collection_inputs() {
        let hash = Hash::new(b"collection");
        let label = z32::encode(hash.as_bytes());
        let url = format!("https://{label}.blake3.net");
        for input in [
            format!("{url}/file"),
            format!("{url}/?download"),
            format!("{url}/#fragment"),
            format!("{url}:8080/"),
            format!("https://user@{label}.blake3.net/"),
            format!("https://{label}.blake3.net.evil/"),
            format!("https://prefix.{label}.blake3.net/"),
            format!("https://{label}.pkarr.net/"),
            format!("ftp://{label}.blake3.net/"),
            "y".repeat(51),
            "0".repeat(52),
        ] {
            assert!(input.parse::<DownloadSource>().is_err(), "{input}");
        }
        let raw = BlobTicket::new(
            iroh::SecretKey::from_bytes(&[1; 32]).public().into(),
            hash,
            BlobFormat::Raw,
        );
        assert!(raw.to_string().parse::<DownloadSource>().is_err());
        let source = DownloadSource::from(hash);
        let encoded = postcard::to_allocvec(&source).unwrap();
        assert_eq!(
            postcard::from_bytes::<DownloadSource>(&encoded).unwrap(),
            source
        );
    }
}
