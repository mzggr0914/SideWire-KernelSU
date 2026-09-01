import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('    HelloAck, ProxyStartAck, ProxyStartRequest, ProxyTokenMode, decode, frame, raw_frame,\n    read_frame, write_frame,','    HelloAck, ProxyStartAck, ProxyStartRequest, ProxyTokenMode, PtyExit, PtyOpenAck,\n    PtyOpenRequest, PtyResize, decode, frame, raw_frame, read_frame, write_frame,');
if(!s.includes('use std::fs::File as StdFile;')) s=s.replace('use std::{collections::HashMap, fs, process::Stdio};','use std::{collections::HashMap, fs, process::Stdio};\n#[cfg(target_os = "android")]\nuse std::fs::File as StdFile;\n#[cfg(target_os = "android")]\nuse std::os::fd::{AsRawFd, FromRawFd};');
s=s.replace('            FrameKind::ProxyStartRequest => {','            FrameKind::PtyOpen => {\n                handle_pty(&mut stream, request.stream_id, &request.payload).await?\n            }\n            FrameKind::ProxyStartRequest => {');
const marker='async fn start_proxy(';
if(!s.includes('fn open_pty(')) s=s.replace(marker,fs.readFileSync('scripts/v050-daemon-pty.txt','utf8')+'\n'+marker);
fs.writeFileSync(p,s);
