import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('use std::{ffi::CString, fs, process::Stdio};','use std::{fs, process::Stdio};');
s=s.replace('#[derive(Clone, Copy, ValueEnum)]\nenum Mode {','#[derive(Clone, Copy, Debug, ValueEnum)]\nenum Mode {');
s=s.replace('fn apply_identity(command: &mut Command, identity: ExecIdentity) -> Result<()> {\n    #[cfg(unix)]','fn apply_identity(command: &mut Command, identity: ExecIdentity) -> Result<()> {\n    #[cfg(not(unix))]\n    let _ = (command, identity);\n\n    #[cfg(unix)]');
s=s.replace('fn set_exec_context(context: &str) -> std::io::Result<()> {\n    let value=CString::new(context)','fn set_exec_context(context: &str) -> std::io::Result<()> {\n    use std::ffi::CString;\n    let value=CString::new(context)');
fs.writeFileSync(p,s);
