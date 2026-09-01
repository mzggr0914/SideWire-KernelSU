use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;

pub const DISCOVERY_PORT: u16 = 58320;
pub const DISCOVERY_REQUEST: &[u8] = b"SIDEWIRE_DISCOVER_V2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(pub [u8; 16]);

impl DeviceId {
    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(32);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }

    pub fn short(self) -> String {
        self.to_hex()[..8].to_owned()
    }
    pub fn parse(value: &str) -> Result<Self> {
        let compact: String = value.chars().filter(|ch| *ch != '-').collect();
        if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("device id must contain 32 hexadecimal digits");
        }
        let mut bytes = [0u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            let text = &compact[offset..offset + 2];
            *byte = u8::from_str_radix(text, 16).context("decode device id")?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryReply {
    pub device_id: DeviceId,
    pub name: String,
    pub port: u16,
    pub protocol_version: u16,
}

#[cfg(test)]
mod tests {
    use super::DeviceId;

    #[test]
    fn device_id_round_trips_hex_and_uuid_style() {
        let id = DeviceId::parse("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        assert_eq!(id.to_hex(), "00112233445566778899aabbccddeeff");
        assert_eq!(id.short(), "00112233");
    }

    #[test]
    fn rejects_invalid_device_id() {
        assert!(DeviceId::parse("not-a-device-id").is_err());
    }
}
