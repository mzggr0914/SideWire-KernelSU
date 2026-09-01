import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
const a=s.indexOf('fn apply_identity(');
const b=s.indexOf('\nasync fn handle_exec',a);
if(a<0||b<0) throw new Error('identity block markers not found');
const identity=`#[cfg(target_os = "android")]
fn apply_identity_now(identity: ExecIdentity) -> std::io::Result<()> {
    if matches!(identity, ExecIdentity::Root) { return Ok(()); }
    const CONTEXT: &[u8] = b"u:r:shell:s0";
    const GROUPS: [libc::gid_t; 15] = [1004,1007,1011,1015,1028,1078,1079,2000,3001,3002,3003,3006,3009,3011,3012];
    let fd = unsafe { libc::open(c"/proc/self/attr/exec".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error()); }
    let rc = unsafe { libc::write(fd, CONTEXT.as_ptr().cast(), CONTEXT.len()) };
    let saved = if rc < 0 { Some(std::io::Error::last_os_error()) } else { None };
    unsafe { libc::close(fd); }
    if let Some(e) = saved { return Err(e); }
    if unsafe { libc::setgroups(GROUPS.len(), GROUPS.as_ptr()) } != 0 { return Err(std::io::Error::last_os_error()); }
    if unsafe { libc::setgid(2000) } != 0 { return Err(std::io::Error::last_os_error()); }
    if unsafe { libc::setuid(2000) } != 0 { return Err(std::io::Error::last_os_error()); }
    Ok(())
}

fn apply_identity(command: &mut Command, identity: ExecIdentity) -> Result<()> {
    #[cfg(target_os = "android")]
    unsafe { command.pre_exec(move || apply_identity_now(identity)); }
    #[cfg(not(target_os = "android"))]
    { let _ = (command, identity); }
    Ok(())
}
`;
s=s.slice(0,a)+identity+s.slice(b);
fs.writeFileSync(p,s);
