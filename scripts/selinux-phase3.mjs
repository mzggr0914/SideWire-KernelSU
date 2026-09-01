import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('    let cli = Cli::parse();\n    #[cfg(target_os = "android")]\n    enter_daemon_context()?;\n    let resolved = resolve_config(&cli)?;', '    let cli = Cli::parse();\n    let resolved = resolve_config(&cli)?;\n    #[cfg(target_os = "android")]\n    enter_daemon_context()?;');
const marker='async fn run_inbound(bind: &str, name: &str) -> Result<()> {';
const fn=`#[cfg(target_os = "android")]\nfn enter_daemon_context() -> Result<()> {\n    use std::ffi::CString;\n    let context = CString::new("u:r:sidewire_daemon:s0")?;\n    let fd = unsafe { libc::open(c"/proc/self/attr/current".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };\n    if fd < 0 { return Err(std::io::Error::last_os_error()).context("open SELinux current context"); }\n    let bytes = context.as_bytes_with_nul();\n    let rc = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len() - 1) };\n    let saved = if rc < 0 { Some(std::io::Error::last_os_error()) } else { None };\n    unsafe { libc::close(fd); }\n    if let Some(error) = saved { return Err(error).context("enter u:r:sidewire_daemon:s0"); }\n    Ok(())\n}\n\n`;
if(!s.includes('fn enter_daemon_context()')) s=s.replace(marker,fn+marker);
fs.writeFileSync(p,s);
