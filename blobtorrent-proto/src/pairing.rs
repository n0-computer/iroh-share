//! Single-client enrollment on the daemon's shared iroh endpoint.
use iroh::EndpointAddr;
use iroh_tickets::{endpoint::EndpointTicket, ParseError, Ticket};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use subtle::ConstantTimeEq;

/// One bidirectional stream: 32 secret bytes followed by FIN, then one status byte.
/// The server authorizes the authenticated connection identity, not a supplied ID.
pub const PAIRING_ALPN: &[u8] = b"/blobtorrent/pair/1";

#[derive(Clone, Serialize, Deserialize)]
pub struct PairingSecret([u8; 32]);
impl PairingSecret {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn matches(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}
impl fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingSecret([redacted])")
    }
}

/// A bearer invitation for one client. Displaying it reveals the enrollment secret.
#[derive(Debug, Clone)]
pub struct PairingTicket {
    pub addr: EndpointAddr,
    pub secret: PairingSecret,
}
/// Postcard discriminants identify fixed layouts; append variants to extend the format.
#[derive(Serialize, Deserialize)]
enum TicketWireFormat {
    V0(PairingTicketV0),
}

#[derive(Serialize, Deserialize)]
struct PairingTicketV0 {
    /// Versioned endpoint-ticket bytes, independent of EndpointAddr's serde layout.
    endpoint: Vec<u8>,
    secret: [u8; 32],
}

impl PairingTicket {
    fn to_wire(&self) -> TicketWireFormat {
        TicketWireFormat::V0(PairingTicketV0 {
            endpoint: EndpointTicket::new(self.addr.clone()).encode_bytes(),
            secret: *self.secret.as_bytes(),
        })
    }
    fn from_wire(wire: TicketWireFormat) -> Result<Self, ParseError> {
        let TicketWireFormat::V0(data) = wire;
        Ok(Self {
            addr: EndpointTicket::decode_bytes(&data.endpoint)?.into(),
            secret: PairingSecret::from_bytes(data.secret),
        })
    }
}

impl Ticket for PairingTicket {
    const KIND: &'static str = "blobtorrent";

    fn encode_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(&self.to_wire()).expect("pairing ticket serialization")
    }
    fn decode_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() > 4096 {
            return Err(ParseError::verification_failed("pairing ticket too large"));
        }
        let (wire, rest) = postcard::take_from_bytes(bytes)?;
        if !rest.is_empty() {
            return Err(ParseError::verification_failed(
                "trailing pairing ticket data",
            ));
        }
        Self::from_wire(wire)
    }
}
impl fmt::Display for PairingTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode_string())
    }
}
impl FromStr for PairingTicket {
    type Err = ParseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > 8192 {
            return Err(ParseError::verification_failed("pairing ticket too large"));
        }
        Self::decode_string(value)
    }
}
impl Serialize for PairingTicket {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.encode_string())
        } else {
            self.to_wire().serialize(serializer)
        }
    }
}
impl<'de> Deserialize<'de> for PairingTicket {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            String::deserialize(deserializer)?
                .parse()
                .map_err(serde::de::Error::custom)
        } else {
            Self::from_wire(TicketWireFormat::deserialize(deserializer)?)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PairingStatus {
    Accepted = 0,
    Invalid = 1,
    StorageFailure = 2,
}

/// Issue an independent invitation for one client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePairingTicket {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_roundtrip_and_debug_redaction() {
        let ticket = PairingTicket {
            addr: EndpointAddr::new(iroh::SecretKey::from_bytes(&[1; 32]).public())
                .with_ip_addr("127.0.0.1:1234".parse().unwrap()),
            secret: PairingSecret::from_bytes([123; 32]),
        };
        let encoded = ticket.to_string();
        let decoded: PairingTicket = encoded.parse().unwrap();
        assert_eq!(decoded.addr, ticket.addr);
        assert!(decoded.secret.matches(&ticket.secret));
        assert!(!format!("{ticket:?}").contains("123, 123"));
        assert!("blobtorrentgarbage!".parse::<PairingTicket>().is_err());
        assert!(format!("{encoded}AAAA").parse::<PairingTicket>().is_err());
    }
    #[test]
    fn version_zero_wire_layout_and_unknown_versions() {
        let ticket = PairingTicket {
            addr: EndpointAddr::new(
                "ae58ff8833241ac82d6ff7611046ed67b5072d142c588d0063e942d9a75502b6"
                    .parse()
                    .unwrap(),
            ),
            secret: PairingSecret::from_bytes([123; 32]),
        };
        // V0, 34-byte endpoint ticket, endpoint variant zero, identity, no addresses, secret.
        let mut expected = vec![0, 34, 0];
        expected.extend_from_slice(ticket.addr.id.as_bytes());
        expected.push(0);
        expected.extend_from_slice(&[123; 32]);
        assert_eq!(ticket.encode_bytes(), expected);
        assert_eq!(postcard::to_allocvec(&ticket).unwrap(), expected);
        let decoded: PairingTicket = postcard::from_bytes(&expected).unwrap();
        assert_eq!(decoded.to_string(), ticket.to_string());
        let json = serde_json::to_string(&ticket).unwrap();
        assert_eq!(json, format!("\"{}\"", ticket));
        assert_eq!(
            serde_json::from_str::<PairingTicket>(&json)
                .unwrap()
                .to_string(),
            ticket.to_string()
        );
        assert!(PairingTicket::decode_bytes(&[1]).is_err());
        expected.push(0);
        assert!(PairingTicket::decode_bytes(&expected).is_err());
        assert!(format!("endpoint{}", ticket)
            .parse::<PairingTicket>()
            .is_err());
    }
}
