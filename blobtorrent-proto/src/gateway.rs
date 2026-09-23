use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, SocketAddrV4};

/// Desired gateway configuration, persisted by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub listen: SocketAddr,
    /// None discovers index servers through Mainline rendezvous.
    pub index_server: Option<SocketAddrV4>,
}
impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: ([127, 0, 0, 1], 8080).into(),
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
