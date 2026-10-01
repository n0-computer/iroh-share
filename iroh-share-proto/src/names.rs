use crate::{JobError, Url};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A pkarr public key, encoded as canonical z-base-32 for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameKey(pub [u8; 32]);
impl fmt::Display for NameKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&z32::encode(&self.0))
    }
}
impl FromStr for NameKey {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || "invalid canonical z-base-32 pkarr public key".to_owned();
        let bytes: [u8; 32] = z32::decode(value.as_bytes())
            .map_err(|_| invalid())?
            .try_into()
            .map_err(|_| invalid())?;
        if z32::encode(&bytes) != value {
            return Err(invalid());
        }
        Ok(Self(bytes))
    }
}
impl NameKey {
    pub fn url(self) -> Url {
        format!("https://{self}.pkarr.net/")
            .parse()
            .expect("public key is a valid hostname")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NameTarget {
    Url(Url),
    Job(u64),
    /// DNS zone-style records, one record per line, relative to this name.
    Records(String),
}

impl NameTarget {
    /// Content names belong with Data, including fixed content URLs with subpaths.
    pub fn is_content(&self) -> bool {
        match self {
            Self::Records(_) => false,
            Self::Job(_) => true,
            Self::Url(url) => url.host_str().is_some_and(|host| {
                host.strip_suffix(".blake3.net").is_some_and(|hash| {
                    format!("https://{hash}.blake3.net/")
                        .parse::<crate::DownloadSource>()
                        .is_ok()
                })
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NameState {
    Disabled,
    WaitingForJob,
    Publishing {
        url: Url,
    },
    Published {
        url: Url,
        sequence: i64,
    },
    Failed {
        error: JobError,
    },
    PublishingRecords,
    PublishedRecords {
        sequence: i64,
    },
    /// The name has a key but no records; it publishes once records are set.
    NoRecords,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Name {
    pub label: String,
    pub key: NameKey,
    pub target: NameTarget,
    pub state: NameState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateName {
    pub label: String,
    pub target: NameTarget,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateName {
    pub label: String,
    pub target: NameTarget,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveName {
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListNames {}

/// What happened to one key of an imported archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedName {
    pub key: NameKey,
    pub outcome: ImportOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportOutcome {
    Imported { label: String },
    Skipped { reason: String },
}

/// Editable HTTPS origin alias; preserve full-URL targets as URI records.
pub fn redirect_records(url: &Url) -> String {
    let mut records = String::new();
    if url.scheme() == "https"
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
    {
        if let Some(host) = url.domain() {
            if let Some(port) = url.port() {
                records.push_str(&format!("@ 300 IN HTTPS 1 {host}. port={port}\n"));
            } else {
                records.push_str(&format!("@ 300 IN HTTPS 0 {host}.\n"));
            }
        }
    }
    if records.is_empty() {
        records = format!(r#"_https._tcp 300 IN URI 0 0 "{url}""#) + "\n";
    }
    records
}

#[cfg(test)]
mod redirect_tests {
    use super::*;
    #[test]
    fn origin_redirect_includes_gateway_https_target() {
        let text = redirect_records(&"https://example.com/".parse().unwrap());
        assert_eq!(text, "@ 300 IN HTTPS 0 example.com.\n");
        assert!(
            redirect_records(&"https://example.com:8443/".parse().unwrap())
                .contains("@ 300 IN HTTPS 1 example.com. port=8443")
        );
        assert!(
            !redirect_records(&"https://example.com/path".parse().unwrap()).contains(" IN HTTPS ")
        );
    }
}
