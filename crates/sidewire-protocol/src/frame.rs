use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::IoSlice;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAGIC: [u8; 4] = *b"SIDE";
pub const PROTOCOL_MAJOR: u8 = 1;
pub const PROTOCOL_MINOR: u8 = 1;
pub const VERSION: u16 = ((PROTOCOL_MAJOR as u16) << 8) | PROTOCOL_MINOR as u16;

pub const fn protocol_major(version: u16) -> u8 {
    (version >> 8) as u8
}
pub const fn protocol_minor(version: u16) -> u8 {
    version as u8
}
pub const fn protocol_compatible(version: u16) -> bool {
    protocol_major(version) == PROTOCOL_MAJOR
}
pub const fn negotiated_version(peer: u16) -> Option<u16> {
    if protocol_compatible(peer) {
        Some(
            ((PROTOCOL_MAJOR as u16) << 8)
                | (if PROTOCOL_MINOR < protocol_minor(peer) {
                    PROTOCOL_MINOR
                } else {
                    protocol_minor(peer)
                }) as u16,
        )
    } else {
        None
    }
}
pub fn protocol_label(version: u16) -> String {
    format!("{}.{}", protocol_major(version), protocol_minor(version))
}
pub const HEADER_LEN: usize = 16;
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum FrameKind {
    Hello = 1,
    HelloAck = 2,
    ExecRequest = 10,
    ExecStdout = 11,
    ExecStderr = 12,
    ExecExit = 13,
    PushRequest = 30,
    FileChunk = 31,
    FileEnd = 32,
    PullRequest = 33,
    FileMeta = 34,
    ProxyStartRequest = 40,
    ProxyStartAck = 41,
    PtyOpen = 50,
    PtyOpenAck = 51,
    PtyInput = 52,
    PtyOutput = 53,
    PtyResize = 54,
    PtyExit = 55,
    PtyClose = 56,
    PtyComplete = 57,
    PtyCompleteResult = 58,
    ClipboardGet = 60,
    ClipboardSet = 61,
    ClipboardClear = 62,
    ClipboardData = 63,
    Ping = 20,
    Pong = 21,
    StreamCancel = 22,
    Error = 255,
}

impl TryFrom<u16> for FrameKind {
    type Error = anyhow::Error;

    fn try_from(value: u16) -> Result<Self> {
        Ok(match value {
            1 => Self::Hello,
            2 => Self::HelloAck,
            10 => Self::ExecRequest,
            11 => Self::ExecStdout,
            12 => Self::ExecStderr,
            13 => Self::ExecExit,
            30 => Self::PushRequest,
            31 => Self::FileChunk,
            32 => Self::FileEnd,
            33 => Self::PullRequest,
            34 => Self::FileMeta,
            40 => Self::ProxyStartRequest,
            41 => Self::ProxyStartAck,
            50 => Self::PtyOpen,
            51 => Self::PtyOpenAck,
            52 => Self::PtyInput,
            53 => Self::PtyOutput,
            54 => Self::PtyResize,
            55 => Self::PtyExit,
            56 => Self::PtyClose,
            57 => Self::PtyComplete,
            58 => Self::PtyCompleteResult,
            60 => Self::ClipboardGet,
            61 => Self::ClipboardSet,
            62 => Self::ClipboardClear,
            63 => Self::ClipboardData,
            20 => Self::Ping,
            21 => Self::Pong,
            22 => Self::StreamCancel,
            255 => Self::Error,
            other => bail!("unknown frame kind {other}"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub kind: FrameKind,
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_allocvec(value).context("serialize protocol payload")
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    postcard::from_bytes(bytes).context("deserialize protocol payload")
}

fn frame_header(kind: FrameKind, stream_id: u32, payload_len: usize) -> Result<[u8; HEADER_LEN]> {
    if payload_len > MAX_PAYLOAD {
        bail!("payload too large: {payload_len} bytes");
    }
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&VERSION.to_be_bytes());
    header[6..8].copy_from_slice(&(kind as u16).to_be_bytes());
    header[8..12].copy_from_slice(&stream_id.to_be_bytes());
    header[12..16].copy_from_slice(&(payload_len as u32).to_be_bytes());
    Ok(header)
}

pub fn encode_frame_bytes(frame: &Frame) -> Result<Vec<u8>> {
    let header = frame_header(frame.kind, frame.stream_id, frame.payload.len())?;
    let mut bytes = Vec::with_capacity(HEADER_LEN + frame.payload.len());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&frame.payload);
    Ok(bytes)
}

pub fn decode_frame_bytes(bytes: &[u8]) -> Result<Frame> {
    if bytes.len() < HEADER_LEN {
        bail!("truncated SideWire frame");
    }
    let header = &bytes[..HEADER_LEN];
    if header[0..4] != MAGIC {
        bail!("invalid SideWire frame magic");
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if !protocol_compatible(version) {
        bail!("unsupported protocol version {}", protocol_label(version));
    }
    let kind = FrameKind::try_from(u16::from_be_bytes([header[6], header[7]]))?;
    let stream_id = u32::from_be_bytes(header[8..12].try_into().unwrap());
    let payload_len = u32::from_be_bytes(header[12..16].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        bail!("payload too large: {payload_len} bytes");
    }
    if bytes.len() != HEADER_LEN + payload_len {
        bail!("invalid SideWire frame length");
    }
    Ok(Frame {
        kind,
        stream_id,
        payload: bytes[HEADER_LEN..].to_vec(),
    })
}

pub async fn write_raw_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    kind: FrameKind,
    stream_id: u32,
    payload: &[u8],
) -> Result<()> {
    let header = frame_header(kind, stream_id, payload.len())?;
    let mut header_offset = 0usize;
    let mut payload_offset = 0usize;

    while header_offset < HEADER_LEN {
        let parts = [
            IoSlice::new(&header[header_offset..]),
            IoSlice::new(&payload[payload_offset..]),
        ];
        let written = writer.write_vectored(&parts).await?;
        if written == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
        }
        let header_left = HEADER_LEN - header_offset;
        if written < header_left {
            header_offset += written;
            continue;
        }
        header_offset = HEADER_LEN;
        payload_offset += written - header_left;
    }

    if payload_offset < payload.len() {
        writer.write_all(&payload[payload_offset..]).await?;
    }
    Ok(())
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> Result<()> {
    write_raw_frame(writer, frame.kind, frame.stream_id, &frame.payload).await
}
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    if header[0..4] != MAGIC {
        bail!("invalid SideWire frame magic");
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if !protocol_compatible(version) {
        bail!("unsupported protocol version {}", protocol_label(version));
    }
    let kind = FrameKind::try_from(u16::from_be_bytes([header[6], header[7]]))?;
    let stream_id = u32::from_be_bytes(header[8..12].try_into().unwrap());
    let payload_len = u32::from_be_bytes(header[12..16].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        bail!("payload too large: {payload_len} bytes");
    }
    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).await?;
    Ok(Frame {
        kind,
        stream_id,
        payload,
    })
}

pub fn frame<T: Serialize>(kind: FrameKind, stream_id: u32, value: &T) -> Result<Frame> {
    Ok(Frame {
        kind,
        stream_id,
        payload: encode(value)?,
    })
}

pub fn raw_frame(kind: FrameKind, stream_id: u32, payload: Vec<u8>) -> Frame {
    Frame {
        kind,
        stream_id,
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FrameKind, PROTOCOL_MAJOR, PROTOCOL_MINOR, VERSION, negotiated_version,
        protocol_compatible, protocol_label, read_frame, write_raw_frame,
    };

    #[test]
    fn protocol_negotiates_within_major_only() {
        let older_minor = (PROTOCOL_MAJOR as u16) << 8;
        assert!(protocol_compatible(older_minor));
        assert_eq!(negotiated_version(older_minor), Some(older_minor));

        let newer_minor =
            ((PROTOCOL_MAJOR as u16) << 8) | (PROTOCOL_MINOR.saturating_add(3) as u16);
        assert!(protocol_compatible(newer_minor));
        assert_eq!(negotiated_version(newer_minor), Some(VERSION));
        let next_major = ((PROTOCOL_MAJOR + 1) as u16) << 8;
        assert!(!protocol_compatible(next_major));
        assert_eq!(negotiated_version(next_major), None);
        assert_eq!(protocol_label(VERSION), "1.1");
    }

    #[tokio::test]
    async fn vectored_writer_round_trips_payload() {
        let (mut writer, mut reader) = tokio::io::duplex(128);
        let send = async {
            write_raw_frame(&mut writer, FrameKind::PtyInput, 42, b"abc123")
                .await
                .unwrap();
        };
        let receive = async {
            let frame = read_frame(&mut reader).await.unwrap();
            assert_eq!(frame.kind, FrameKind::PtyInput);
            assert_eq!(frame.stream_id, 42);
            assert_eq!(frame.payload, b"abc123");
        };
        tokio::join!(send, receive);
    }

    #[tokio::test]
    async fn vectored_writer_round_trips_empty_payload() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        write_raw_frame(&mut writer, FrameKind::PtyClose, 7, &[])
            .await
            .unwrap();
        let frame = read_frame(&mut reader).await.unwrap();
        assert_eq!(frame.kind, FrameKind::PtyClose);
        assert_eq!(frame.stream_id, 7);
        assert!(frame.payload.is_empty());
    }
}
