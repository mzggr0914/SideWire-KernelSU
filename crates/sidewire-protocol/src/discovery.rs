use serde::{Deserialize, Serialize};

pub const DISCOVERY_PORT: u16 = 58320;
pub const DISCOVERY_REQUEST: &[u8] = b"SIDEWIRE_DISCOVER_V1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryReply {
    pub name: String,
    pub port: u16,
    pub protocol_version: u16,
}
