import fs from "node:fs";

const path = "apps/sidewired/src/main.rs";
let src = fs.readFileSync(path, "utf8");
const startMarker = '#[cfg(target_os = "android")]\nfn open_pty_master';
const endMarker = '#[cfg(target_os = "android")]\nfn configure_pty_child';
const start = src.indexOf(startMarker);
const end = src.indexOf(endMarker, start);
if (start < 0 || end < 0) {
  throw new Error("PTY function markers not found");
}

const replacement = String.raw`#[cfg(target_os = "android")]
fn dup_cloexec(fd: std::os::fd::RawFd) -> std::io::Result<std::os::fd::RawFd> {
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(duplicated)
}

#[cfg(target_os = "android")]
fn open_pty_master(
    cols: u16,
    rows: u16,
) -> Result<(tokio::fs::File, tokio::fs::File, StdFile, std::ffi::CString)> {
    // Tokio fs::File serializes read/write through one Busy state.
    // Keep independent file objects for PTY input and output.
    let master_fd = unsafe {
        libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC)
    };
    if master_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("posix_openpt");
    }
    if unsafe { libc::grantpt(master_fd) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(master_fd); }
        return Err(error).context("grantpt");
    }
    if unsafe { libc::unlockpt(master_fd) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(master_fd); }
        return Err(error).context("unlockpt");
    }
    let mut name = [0 as libc::c_char; 128];
    let rc = unsafe { libc::ptsname_r(master_fd, name.as_mut_ptr(), name.len()) };
    if rc != 0 {
        unsafe { libc::close(master_fd); }
        return Err(std::io::Error::from_raw_os_error(rc)).context("ptsname_r");
    }
    if let Err(error) = set_pty_size(master_fd, cols, rows) {
        unsafe { libc::close(master_fd); }
        return Err(error).context("set initial PTY size");
    }
    let write_fd = match dup_cloexec(master_fd) {
        Ok(fd) => fd,
        Err(error) => {
            unsafe { libc::close(master_fd); }
            return Err(error).context("duplicate PTY master for writes");
        }
    };
    let resize_fd = match dup_cloexec(master_fd) {
        Ok(fd) => fd,
        Err(error) => {
            unsafe {
                libc::close(master_fd);
                libc::close(write_fd);
            }
            return Err(error).context("duplicate PTY master for resize");
        }
    };
    let slave_name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()).to_owned() };
    let read_file = unsafe { StdFile::from_raw_fd(master_fd) };
    let write_file = unsafe { StdFile::from_raw_fd(write_fd) };
    let resize_file = unsafe { StdFile::from_raw_fd(resize_fd) };
    Ok((
        tokio::fs::File::from_std(read_file),
        tokio::fs::File::from_std(write_file),
        resize_file,
        slave_name,
    ))
}

`;
src = src.slice(0, start) + replacement + src.slice(end);
const tupleOld = `        let (master, resize, slave_name) =
            open_pty_master(request.cols.max(1), request.rows.max(1))?;`;
const tupleNew = `        let (mut master_read, mut master_write, resize, slave_name) =
            open_pty_master(request.cols.max(1), request.rows.max(1))?;`;
if (!src.includes(tupleOld)) throw new Error("PTY tuple marker missing");
src = src.replace(tupleOld, tupleNew);

const splitOld = `        let (mut master_read, mut master_write) = tokio::io::split(master);
        let (mut net_read, mut net_write) = tokio::io::split(stream);`;
const splitNew = `        let (mut net_read, mut net_write) = tokio::io::split(stream);`;
if (!src.includes(splitOld)) throw new Error("PTY split marker missing");
src = src.replace(splitOld, splitNew);

const inputOld = `                            FrameKind::PtyInput => master_write.write_all(&incoming.payload).await?,`;
const inputNew = `                            FrameKind::PtyInput => {
                                master_write.write_all(&incoming.payload).await?;
                                master_write.flush().await?;
                            }`;
if (!src.includes(inputOld)) throw new Error("PTY input marker missing");
src = src.replace(inputOld, inputNew);
fs.writeFileSync(path, src);
let cargo = fs.readFileSync("Cargo.toml", "utf8");
if (!cargo.includes('version = "0.5.4"')) throw new Error("workspace version marker missing");
cargo = cargo.replace('version = "0.5.4"', 'version = "0.5.5"');
fs.writeFileSync("Cargo.toml", cargo);

let prop = fs.readFileSync("module/module.prop", "utf8");
prop = prop.replace(/^version=.*$/m, "version=0.5.5");
prop = prop.replace(/^versionCode=.*$/m, "versionCode=11");
fs.writeFileSync("module/module.prop", prop);

let customize = fs.readFileSync("module/customize.sh", "utf8");
customize = customize.replace(/SideWire 0\.5\.4/g, "SideWire 0.5.5");
fs.writeFileSync("module/customize.sh", customize);
console.log("Applied v0.5.5 PTY duplex-FD fix");
