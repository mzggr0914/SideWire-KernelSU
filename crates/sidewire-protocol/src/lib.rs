use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAGIC: [u8; 4] = *b"SIDE";
pub const VERSION: u16 = 5;
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
    Ping = 20,
    Pong = 21,
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
            20 => Self::Ping,
            21 => Self::Pong,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub name: String,
    pub role: PeerRole,
    pub protocol_version: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum PeerRole {
    Host,
    Device,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HelloAck {
    pub name: String,
    pub os: String,
    pub arch: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ExecIdentity {
    Root,
    Shell,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub identity: ExecIdentity,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecExit {
    pub code: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FilePushRequest {
    pub path: String,
    pub identity: ExecIdentity,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FilePullRequest {
    pub path: String,
    pub identity: ExecIdentity,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileMeta {
    pub size: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ProxyTokenMode {
    Expect,
    Send,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProxyStartRequest {
    pub id: String,
    pub bind: String,
    pub target: String,
    pub token: Vec<u8>,
    pub token_mode: ProxyTokenMode,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProxyStartAck {
    pub id: String,
    pub bound: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyOpenRequest {
    pub program: String,
    pub args: Vec<String>,
    pub identity: ExecIdentity,
    pub cols: u16,
    pub rows: u16,
    pub term: String,
    pub echo: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyOpenAck {
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PtyResize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyCompleteRequest {
    pub line: String,
    pub cursor: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyCompleteResult {
    pub line: String,
    pub cursor: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyExit {
    pub code: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub message: String,
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_allocvec(value).context("serialize protocol payload")
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    postcard::from_bytes(bytes).context("deserialize protocol payload")
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> Result<()> {
    if frame.payload.len() > MAX_PAYLOAD {
        bail!("payload too large: {} bytes", frame.payload.len());
    }
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&VERSION.to_be_bytes());
    header[6..8].copy_from_slice(&(frame.kind as u16).to_be_bytes());
    header[8..12].copy_from_slice(&frame.stream_id.to_be_bytes());
    header[12..16].copy_from_slice(&(frame.payload.len() as u32).to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(&frame.payload).await?;
    writer.flush().await?;
    Ok(())
}
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    if header[0..4] != MAGIC {
        bail!("invalid SideWire frame magic");
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if version != VERSION {
        bail!("unsupported protocol version {version}");
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
