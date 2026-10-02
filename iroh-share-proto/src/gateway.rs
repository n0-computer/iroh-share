//! Gateway messages, kept so the control protocol stays wire-compatible with
//! clients and daemons from 0.1.7 and earlier.
//!
//! The daemon no longer runs a gateway. It answers `GetGateway` with a disabled
//! snapshot and rejects `SetGateway`; clients ignore `GatewayUpdated`.
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, SocketAddrV4};

/// Desired gateway configuration, persisted by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub listen: SocketAddr,
    /// None uses the index servers listed by n0.
    pub index_server: Option<SocketAddrV4>,
}
impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: ([127, 0, 0, 1], 45475).into(),
            index_server: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayState {
    Disabled,
    Starting,
    Running {
        listen: SocketAddr,
        endpoint: crate::EndpointId,
    },
    Failed {
        error: String,
    },
}

/// Desired configuration and observed runtime state are reported together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewaySnapshot {
    pub config: GatewayConfig,
    pub state: GatewayState,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetGateway {}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetGateway {
    pub config: GatewayConfig,
}
