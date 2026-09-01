import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('    HelloAck, ProxyStartAck, ProxyStartRequest, ProxyTokenMode, PtyExit, PtyOpenAck,\n    PtyOpenRequest, PtyResize, decode, frame, raw_frame, read_frame, write_frame,','    HelloAck, ProxyStartAck, ProxyStartRequest, ProxyTokenMode, PtyOpenRequest, decode, frame,\n    raw_frame, read_frame, write_frame,');
if(!s.includes('use sidewire_protocol::{PtyExit, PtyOpenAck, PtyResize};')) s=s.replace('use std::{collections::HashMap, fs, process::Stdio};','use std::{collections::HashMap, fs, process::Stdio};\n#[cfg(target_os = "android")]\nuse sidewire_protocol::{PtyExit, PtyOpenAck, PtyResize};');
fs.writeFileSync(p,s);
