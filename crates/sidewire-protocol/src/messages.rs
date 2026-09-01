use crate::DeviceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub device_id: Option<DeviceId>,
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
    pub device_id: Option<DeviceId>,
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
    pub candidates: Vec<String>,
    pub candidate_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PtyExit {
    pub code: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub message: String,
}
