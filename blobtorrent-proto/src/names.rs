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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NameState {
    Disabled,
    WaitingForJob,
    Publishing { url: Url },
    Published { url: Url, sequence: i64 },
    Failed { error: JobError },
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
