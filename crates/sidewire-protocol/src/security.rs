use crate::frame::{decode_frame_header, frame_header};
use crate::{Frame, FrameKind, HEADER_LEN};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use snow::TransportState;
use std::{io::IoSlice, sync::Arc};
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
    pub protocol_version: u16,
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
    let header = (bytes.len() as u32).to_be_bytes();
    let mut header_offset = 0usize;
    let mut bytes_offset = 0usize;
    while header_offset < header.len() {
        let parts = [
            IoSlice::new(&header[header_offset..]),
            IoSlice::new(&bytes[bytes_offset..]),
        ];
        let written = writer.write_vectored(&parts).await?;
        if written == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
        }
        let header_left = header.len() - header_offset;
        if written < header_left {
            header_offset += written;
            continue;
        }
        header_offset = header.len();
        bytes_offset += written - header_left;
    }
    if bytes_offset < bytes.len() {
        writer.write_all(&bytes[bytes_offset..]).await?;
    }
    writer.flush().await?;
    Ok(())
}

async fn read_raw_packet_into<R: AsyncRead + Unpin>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
) -> Result<usize> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header).await?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_PACKET {
        bail!("security packet too large: {len} bytes");
    }
    bytes.resize(len, 0);
    reader.read_exact(bytes).await?;
    Ok(len)
}

pub async fn read_raw_packet<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    read_raw_packet_into(reader, &mut bytes).await?;
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

async fn write_noise_record_with_buffer<W: AsyncWrite + Unpin>(
    writer: &mut W,
    noise: &SharedNoise,
    plaintext: &[u8],
    output: &mut Vec<u8>,
) -> Result<()> {
    if plaintext.len() > NOISE_CHUNK + 1 {
        bail!(
            "Noise plaintext record too large: {} bytes",
            plaintext.len()
        );
    }
    output.resize(plaintext.len() + 32, 0);
    let written = {
        let mut state = noise.lock().await;
        state.write_message(plaintext, output)?
    };
    write_raw_packet(writer, &output[..written]).await
}

pub async fn write_noise_record<W: AsyncWrite + Unpin>(
    writer: &mut W,
    noise: &SharedNoise,
    plaintext: &[u8],
) -> Result<()> {
    let mut output = Vec::new();
    write_noise_record_with_buffer(writer, noise, plaintext, &mut output).await
}

async fn read_noise_record_into<R: AsyncRead + Unpin>(
    reader: &mut R,
    noise: &SharedNoise,
    encrypted: &mut Vec<u8>,
    plaintext: &mut Vec<u8>,
) -> Result<usize> {
    let encrypted_len = read_raw_packet_into(reader, encrypted).await?;
    plaintext.resize(encrypted_len, 0);
    let written = {
        let mut state = noise.lock().await;
        state.read_message(&encrypted[..encrypted_len], plaintext)?
    };
    plaintext.truncate(written);
    Ok(written)
}

pub async fn read_noise_record<R: AsyncRead + Unpin>(
    reader: &mut R,
    noise: &SharedNoise,
) -> Result<Vec<u8>> {
    let mut encrypted = Vec::new();
    let mut plaintext = Vec::new();
    read_noise_record_into(reader, noise, &mut encrypted, &mut plaintext).await?;
    Ok(plaintext)
}

pub struct SecureFrameReader<R> {
    inner: R,
    noise: Option<SharedNoise>,
    encrypted_scratch: Vec<u8>,
    plaintext_scratch: Vec<u8>,
}

impl<R> SecureFrameReader<R> {
    pub fn new(inner: R, noise: Option<SharedNoise>) -> Self {
        Self {
            inner,
            noise,
            encrypted_scratch: Vec::with_capacity(NOISE_CHUNK + 32),
            plaintext_scratch: Vec::with_capacity(NOISE_CHUNK + 32),
        }
    }
}

impl<R: AsyncRead + Unpin> SecureFrameReader<R> {
    pub async fn read_frame(&mut self) -> Result<Frame> {
        let Some(noise) = self.noise.clone() else {
            return crate::read_frame(&mut self.inner).await;
        };
        let mut header = [0u8; HEADER_LEN];
        let mut header_filled = 0usize;
        let mut metadata = None;
        let mut payload = Vec::new();
        loop {
            read_noise_record_into(
                &mut self.inner,
                &noise,
                &mut self.encrypted_scratch,
                &mut self.plaintext_scratch,
            )
            .await?;
            let (&final_flag, mut bytes) = self
                .plaintext_scratch
                .split_first()
                .context("empty encrypted SideWire record")?;
            if final_flag > 1 {
                bail!("invalid encrypted SideWire record flag {final_flag}");
            }
            if header_filled < HEADER_LEN {
                let take = (HEADER_LEN - header_filled).min(bytes.len());
                header[header_filled..header_filled + take].copy_from_slice(&bytes[..take]);
                header_filled += take;
                bytes = &bytes[take..];
                if header_filled == HEADER_LEN {
                    let decoded = decode_frame_header(&header)?;
                    payload = Vec::with_capacity(decoded.2);
                    metadata = Some(decoded);
                }
            }
            if let Some((_, _, expected)) = metadata {
                if payload.len() + bytes.len() > expected {
                    bail!("encrypted SideWire frame exceeded declared payload length");
                }
                payload.extend_from_slice(bytes);
                if final_flag == 0 && payload.len() == expected {
                    bail!("encrypted SideWire frame missing final record flag");
                }
            }
            if final_flag == 1 {
                let Some((kind, stream_id, expected)) = metadata else {
                    bail!("truncated encrypted SideWire frame header");
                };
                if payload.len() != expected {
                    bail!("invalid SideWire frame length");
                }
                return Ok(Frame {
                    kind,
                    stream_id,
                    payload,
                });
            }
        }
    }
}

pub struct SecureFrameWriter<W> {
    inner: W,
    noise: Option<SharedNoise>,
    plain_scratch: Vec<u8>,
    cipher_scratch: Vec<u8>,
}

impl<W> SecureFrameWriter<W> {
    pub fn new(inner: W, noise: Option<SharedNoise>) -> Self {
        Self {
            inner,
            noise,
            plain_scratch: Vec::with_capacity(NOISE_CHUNK + 1),
            cipher_scratch: Vec::with_capacity(NOISE_CHUNK + 33),
        }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: AsyncWrite + Unpin> SecureFrameWriter<W> {
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        self.write_raw(frame.kind, frame.stream_id, &frame.payload)
            .await
    }

    pub async fn write_raw(
        &mut self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        let Some(noise) = self.noise.clone() else {
            return crate::write_raw_frame(&mut self.inner, kind, stream_id, payload).await;
        };
        let header = frame_header(kind, stream_id, payload.len())?;
        let total = HEADER_LEN + payload.len();
        let mut offset = 0usize;
        while offset < total {
            let chunk_len = (total - offset).min(NOISE_CHUNK);
            self.plain_scratch.clear();
            self.plain_scratch
                .push(u8::from(offset + chunk_len == total));
            if offset < HEADER_LEN {
                let header_end = (offset + chunk_len).min(HEADER_LEN);
                self.plain_scratch
                    .extend_from_slice(&header[offset..header_end]);
                let used = header_end - offset;
                let payload_take = chunk_len - used;
                if payload_take > 0 {
                    self.plain_scratch
                        .extend_from_slice(&payload[..payload_take]);
                }
            } else {
                let payload_start = offset - HEADER_LEN;
                self.plain_scratch
                    .extend_from_slice(&payload[payload_start..payload_start + chunk_len]);
            }
            write_noise_record_with_buffer(
                &mut self.inner,
                &noise,
                &self.plain_scratch,
                &mut self.cipher_scratch,
            )
            .await?;
            offset += chunk_len;
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown().await?;
        Ok(())
    }
}

pub fn security_prologue(
    version: u16,
    initiator: crate::DeviceId,
    responder: crate::DeviceId,
) -> Vec<u8> {
    format!(
        "SideWire/v{}/{}->{}",
        version,
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
    tokio::try_join!(upload, download)?;
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
    async fn noise_frame_transport_round_trips_record_boundaries() {
        for size in [
            0usize,
            1,
            NOISE_CHUNK - HEADER_LEN - 1,
            NOISE_CHUNK - HEADER_LEN,
            NOISE_CHUNK - HEADER_LEN + 1,
            NOISE_CHUNK * 2,
        ] {
            let (mut left, mut right) = tokio::io::duplex(512 * 1024);
            let psk = [13u8; 32];
            let (initiator, responder) = tokio::join!(
                noise_initiator(&mut left, &psk, b"boundary-test"),
                noise_responder(&mut right, &psk, b"boundary-test"),
            );
            let payload = vec![0x3cu8; size];
            let mut writer = SecureFrameWriter::new(&mut left, Some(initiator.unwrap()));
            let mut reader = SecureFrameReader::new(&mut right, Some(responder.unwrap()));
            let (sent, received) = tokio::join!(
                writer.write_raw(FrameKind::FileChunk, 88, &payload),
                reader.read_frame(),
            );
            sent.unwrap();
            let frame = received.unwrap();
            assert_eq!(frame.stream_id, 88);
            assert_eq!(frame.payload, payload);
        }
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
        let outgoing = crate::raw_frame(FrameKind::FileChunk, 77, payload.clone());
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

    struct FailingTunnelIo;

    impl AsyncRead for FailingTunnelIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    impl AsyncWrite for FailingTunnelIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn noise_tunnel_propagates_write_error() {
        let (mut left, mut right) = tokio::io::duplex(4096);
        let psk = [11u8; 32];
        let (left_noise, right_noise) = tokio::join!(
            noise_initiator(&mut left, &psk, b"tunnel-error"),
            noise_responder(&mut right, &psk, b"tunnel-error"),
        );
        drop(right_noise.unwrap());
        let noise = left_noise.unwrap();
        let (mut client, mut plain) = tokio::io::duplex(64);
        client.write_all(b"data").await.unwrap();
        let mut failing = FailingTunnelIo;
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            copy_noise_tunnel(&mut plain, &mut failing, noise),
        )
        .await
        .expect("tunnel did not propagate write failure");
        assert!(result.is_err());
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
