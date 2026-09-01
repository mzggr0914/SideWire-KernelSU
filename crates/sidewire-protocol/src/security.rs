use crate::{Frame, FrameKind, decode_frame_bytes, encode_frame_bytes, raw_frame};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use snow::TransportState;
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Mutex,
};

pub const PAIRING_PORT: u16 = 58323;
const MAX_PACKET: usize = 128 * 1024;
const NOISE_CHUNK: usize = 60 * 1024;
const NOISE_PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";

pub type SharedNoise = Arc<Mutex<TransportState>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityMode {
    Secure,
    Insecure,
}

impl SecurityMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Secure => "secure",
            Self::Insecure => "insecure",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityBanner {
    pub node_id: crate::DeviceId,
    pub security: SecurityMode,
    pub protocol_version: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityClientHello {
    pub node_id: crate::DeviceId,
    pub security: SecurityMode,
    pub protocol_version: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityDecision {
    pub accepted: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairBanner {
    pub device_id: crate::DeviceId,
    pub name: String,
    pub protocol_version: u16,
    pub pairing_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairStart {
    pub host_id: crate::DeviceId,
    pub host_name: String,
    pub spake_message: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairReply {
    pub spake_message: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairCommit {
    pub host_id: crate::DeviceId,
    pub host_name: String,
    pub shared_secret: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairComplete {
    pub device_id: crate::DeviceId,
    pub device_name: String,
}

pub async fn write_raw_packet<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PACKET {
        bail!("security packet too large: {} bytes", bytes.len());
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_raw_packet<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header).await?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_PACKET {
        bail!("security packet too large: {len} bytes");
    }
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

pub async fn write_packet<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = crate::encode(value)?;
    write_raw_packet(writer, &bytes).await
}

pub async fn read_packet<R, T>(reader: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let bytes = read_raw_packet(reader).await?;
    crate::decode(&bytes)
}

fn noise_builder<'a>(psk: &'a [u8; 32], prologue: &'a [u8]) -> Result<snow::Builder<'a>> {
    let params = NOISE_PATTERN
        .parse()
        .context("parse SideWire Noise pattern")?;
    Ok(snow::Builder::new(params).prologue(prologue).psk(0, psk))
}

pub async fn noise_initiator<S>(
    stream: &mut S,
    psk: &[u8; 32],
    prologue: &[u8],
) -> Result<SharedNoise>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut state = noise_builder(psk, prologue)?.build_initiator()?;
    let mut output = vec![0u8; 65535];
    let written = state.write_message(&[], &mut output)?;
    write_raw_packet(stream, &output[..written]).await?;
    let incoming = read_raw_packet(stream).await?;
    state.read_message(&incoming, &mut output)?;
    let transport = state.into_transport_mode()?;
    Ok(Arc::new(Mutex::new(transport)))
}

pub async fn noise_responder<S>(
    stream: &mut S,
    psk: &[u8; 32],
    prologue: &[u8],
) -> Result<SharedNoise>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut state = noise_builder(psk, prologue)?.build_responder()?;
    let incoming = read_raw_packet(stream).await?;
    let mut output = vec![0u8; 65535];
    state.read_message(&incoming, &mut output)?;
    let written = state.write_message(&[], &mut output)?;
    write_raw_packet(stream, &output[..written]).await?;
    let transport = state.into_transport_mode()?;
    Ok(Arc::new(Mutex::new(transport)))
}

pub async fn write_noise_record<W: AsyncWrite + Unpin>(
    writer: &mut W,
    noise: &SharedNoise,
    plaintext: &[u8],
) -> Result<()> {
    if plaintext.len() > NOISE_CHUNK + 1 {
        bail!(
            "Noise plaintext record too large: {} bytes",
            plaintext.len()
        );
    }
    let mut output = vec![0u8; plaintext.len() + 32];
    let written = {
        let mut state = noise.lock().await;
        state.write_message(plaintext, &mut output)?
    };
    write_raw_packet(writer, &output[..written]).await
}

pub async fn read_noise_record<R: AsyncRead + Unpin>(
    reader: &mut R,
    noise: &SharedNoise,
) -> Result<Vec<u8>> {
    let encrypted = read_raw_packet(reader).await?;
    let mut plaintext = vec![0u8; encrypted.len()];
    let written = {
        let mut state = noise.lock().await;
        state.read_message(&encrypted, &mut plaintext)?
    };
    plaintext.truncate(written);
    Ok(plaintext)
}

pub struct SecureFrameReader<R> {
    inner: R,
    noise: Option<SharedNoise>,
}

impl<R> SecureFrameReader<R> {
    pub fn new(inner: R, noise: Option<SharedNoise>) -> Self {
        Self { inner, noise }
    }
}

impl<R: AsyncRead + Unpin> SecureFrameReader<R> {
    pub async fn read_frame(&mut self) -> Result<Frame> {
        let Some(noise) = &self.noise else {
            return crate::read_frame(&mut self.inner).await;
        };
        let mut encoded = Vec::new();
        loop {
            let record = read_noise_record(&mut self.inner, noise).await?;
            let (&final_flag, bytes) = record
                .split_first()
                .context("empty encrypted SideWire record")?;
            if final_flag > 1 {
                bail!("invalid encrypted SideWire record flag {final_flag}");
            }
            encoded.extend_from_slice(bytes);
            if encoded.len() > crate::MAX_PAYLOAD + crate::HEADER_LEN {
                bail!("encrypted SideWire frame exceeded maximum size");
            }
            if final_flag == 1 {
                return decode_frame_bytes(&encoded);
            }
        }
    }
}

pub struct SecureFrameWriter<W> {
    inner: W,
    noise: Option<SharedNoise>,
}

impl<W> SecureFrameWriter<W> {
    pub fn new(inner: W, noise: Option<SharedNoise>) -> Self {
        Self { inner, noise }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: AsyncWrite + Unpin> SecureFrameWriter<W> {
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        let Some(noise) = &self.noise else {
            return crate::write_frame(&mut self.inner, frame).await;
        };
        let encoded = encode_frame_bytes(frame)?;
        let chunk_count = encoded.len().div_ceil(NOISE_CHUNK);
        for (index, chunk) in encoded.chunks(NOISE_CHUNK).enumerate() {
            let mut record = Vec::with_capacity(chunk.len() + 1);
            record.push(u8::from(index + 1 == chunk_count));
            record.extend_from_slice(chunk);
            write_noise_record(&mut self.inner, noise, &record).await?;
        }
        Ok(())
    }

    pub async fn write_raw(
        &mut self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        self.write_frame(&raw_frame(kind, stream_id, payload.to_vec()))
            .await
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown().await?;
        Ok(())
    }
}

pub fn security_prologue(initiator: crate::DeviceId, responder: crate::DeviceId) -> Vec<u8> {
    format!(
        "SideWire/v{}/{}->{}",
        crate::VERSION,
        initiator.to_hex(),
        responder.to_hex()
    )
    .into_bytes()
}

pub fn pairing_psk(
    shared: &[u8],
    host_id: crate::DeviceId,
    device_id: crate::DeviceId,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"SideWire pairing v1");
    hasher.update(&host_id.0);
    hasher.update(&device_id.0);
    hasher.update(shared);
    *hasher.finalize().as_bytes()
}

pub fn key_to_hex(key: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    use std::fmt::Write as _;
    for byte in key {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

pub fn key_from_hex(value: &str) -> Result<[u8; 32]> {
    let value = value.trim();
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("security key must contain 64 hexadecimal digits");
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(out)
}
pub fn proxy_prologue(token: &[u8], direction: &str) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"SideWire proxy v1");
    hasher.update(direction.as_bytes());
    hasher.update(token);
    let digest = hasher.finalize();
    format!(
        "SideWire/proxy/v{}/{direction}/{}",
        crate::VERSION,
        digest.to_hex()
    )
    .into_bytes()
}

pub async fn copy_noise_tunnel<P, E>(
    plain: &mut P,
    encrypted: &mut E,
    noise: SharedNoise,
) -> Result<()>
where
    P: AsyncRead + AsyncWrite + Unpin,
    E: AsyncRead + AsyncWrite + Unpin,
{
    let (mut plain_read, mut plain_write) = tokio::io::split(plain);
    let (mut encrypted_read, mut encrypted_write) = tokio::io::split(encrypted);
    let upload_noise = noise.clone();
    let upload = async move {
        let mut buffer = vec![0u8; 48 * 1024];
        loop {
            let read = plain_read.read(&mut buffer).await?;
            if read == 0 {
                write_noise_record(&mut encrypted_write, &upload_noise, &[1]).await?;
                return Ok::<(), anyhow::Error>(());
            }
            let mut record = Vec::with_capacity(read + 1);
            record.push(0);
            record.extend_from_slice(&buffer[..read]);
            write_noise_record(&mut encrypted_write, &upload_noise, &record).await?;
        }
    };
    let download = async move {
        loop {
            let record = read_noise_record(&mut encrypted_read, &noise).await?;
            let (&kind, bytes) = record
                .split_first()
                .context("empty encrypted proxy record")?;
            match kind {
                0 => {
                    plain_write.write_all(bytes).await?;
                    plain_write.flush().await?;
                }
                1 if bytes.is_empty() => {
                    plain_write.shutdown().await?;
                    return Ok::<(), anyhow::Error>(());
                }
                _ => bail!("invalid encrypted proxy record"),
            }
        }
    };
    let (upload, download) = tokio::join!(upload, download);
    upload?;
    download?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_key_hex_round_trips() {
        let key = [0xabu8; 32];
        let encoded = key_to_hex(&key);
        assert_eq!(encoded.len(), 64);
        assert_eq!(key_from_hex(&encoded).unwrap(), key);
    }

    #[test]
    fn pairing_key_binds_both_identities() {
        let host = crate::DeviceId([1; 16]);
        let device = crate::DeviceId([2; 16]);
        assert_eq!(
            pairing_psk(b"shared", host, device),
            pairing_psk(b"shared", host, device)
        );
        assert_ne!(
            pairing_psk(b"shared", host, device),
            pairing_psk(b"shared", device, host)
        );
    }

    #[tokio::test]
    async fn noise_frame_transport_round_trips_large_frame() {
        let (mut left, mut right) = tokio::io::duplex(512 * 1024);
        let psk = [7u8; 32];
        let prologue = b"sidewire-test";
        let (initiator, responder) = tokio::join!(
            noise_initiator(&mut left, &psk, prologue),
            noise_responder(&mut right, &psk, prologue),
        );
        let initiator = initiator.unwrap();
        let responder = responder.unwrap();
        let payload = vec![0x5au8; 170 * 1024];
        let outgoing = raw_frame(FrameKind::FileChunk, 77, payload.clone());
        {
            let mut writer = SecureFrameWriter::new(&mut left, Some(initiator));
            writer.write_frame(&outgoing).await.unwrap();
        }
        let incoming = {
            let mut reader = SecureFrameReader::new(&mut right, Some(responder));
            reader.read_frame().await.unwrap()
        };
        assert_eq!(incoming.kind, FrameKind::FileChunk);
        assert_eq!(incoming.stream_id, 77);
        assert_eq!(incoming.payload, payload);
    }

    #[tokio::test]
    async fn noise_tunnel_preserves_tcp_half_close() {
        let (mut client, mut left_plain) = tokio::io::duplex(4096);
        let (mut left_encrypted, mut right_encrypted) = tokio::io::duplex(64 * 1024);
        let (mut right_plain, mut server) = tokio::io::duplex(4096);
        let psk = [9u8; 32];
        let prologue = b"sidewire-tunnel-test";
        let (left_noise, right_noise) = tokio::join!(
            noise_initiator(&mut left_encrypted, &psk, prologue),
            noise_responder(&mut right_encrypted, &psk, prologue),
        );
        let left_noise = left_noise.unwrap();
        let right_noise = right_noise.unwrap();
        let left_tunnel = copy_noise_tunnel(&mut left_plain, &mut left_encrypted, left_noise);
        let right_tunnel = copy_noise_tunnel(&mut right_plain, &mut right_encrypted, right_noise);
        let client_flow = async {
            client.write_all(b"request").await.unwrap();
            client.shutdown().await.unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            assert_eq!(response, b"response");
        };
        let server_flow = async {
            let mut request = Vec::new();
            server.read_to_end(&mut request).await.unwrap();
            assert_eq!(request, b"request");
            server.write_all(b"response").await.unwrap();
            server.shutdown().await.unwrap();
        };
        let (left, right, (), ()) =
            tokio::join!(left_tunnel, right_tunnel, client_flow, server_flow);
        left.unwrap();
        right.unwrap();
    }
}
