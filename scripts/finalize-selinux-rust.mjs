import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('#[derive(Clone, Copy, Debug, ValueEnum)]\nenum Mode {','#[derive(Clone, Copy, Debug, ValueEnum)]\nenum Mode {');
s=s.replace('    let cli = Cli::parse();\n    let resolved = resolve_config(&cli)?;','    let cli = Cli::parse();\n    #[cfg(target_os = "android")]\n    enter_daemon_context()?;\n    let resolved = resolve_config(&cli)?;');
s=s.replace('#[cfg(unix)]\n    unsafe {','#[cfg(target_os = "android")]\n    unsafe {');
s=s.replace('#[cfg(unix)]\nfn set_exec_context','#[cfg(target_os = "android")]\nfn set_exec_context');
s=s.replace('const GROUPS: [libc::gid_t; 12] = [1007,1011,1015,1028,1078,1079,2000,3001,3002,3003,3006,3009];','const GROUPS: [libc::gid_t; 15] = [1004,1007,1011,1015,1028,1078,1079,2000,3001,3002,3003,3006,3009,3011,3012];');
fs.writeFileSync(p,s);
