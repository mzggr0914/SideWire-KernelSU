use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use sidewire_protocol::{
    ExecExit, ExecIdentity, ExecRequest, FileMeta, FilePullRequest, FilePushRequest, FrameKind,
    HelloAck, ProxyStartAck, ProxyStartRequest, ProxyTokenMode, PtyOpenRequest, decode, frame,
    raw_frame, read_frame, write_frame,
};
#[cfg(target_os = "android")]
use sidewire_protocol::{PtyCompleteRequest, PtyCompleteResult, PtyExit, PtyOpenAck, PtyResize};
#[cfg(target_os = "android")]
use std::fs::File as StdFile;
#[cfg(target_os = "android")]
use std::os::fd::{AsRawFd, FromRawFd};
use std::{collections::HashMap, fs, process::Stdio};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    time::{Duration, sleep},
};

#[derive(Parser)]
#[command(name = "sidewired", version, about = "SideWire native device daemon")]
struct Cli {
    #[arg(long, value_enum, default_value = "inbound")]
    mode: Mode,
    #[arg(long, default_value = "0.0.0.0:58321")]
    listen: String,
    #[arg(long, default_value = "127.0.0.1:58321")]
    server: String,
    #[arg(long, default_value = "sidewire-device")]
    name: String,
    #[arg(long)]
    config: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    Inbound,
    Outbound,
}

#[derive(Clone, Debug)]
struct ResolvedConfig {
    mode: Mode,
    endpoint: String,
    name: String,
}

fn resolve_config(cli: &Cli) -> Result<ResolvedConfig> {
    if let Some(path) = &cli.config {
        let text = fs::read_to_string(path).with_context(|| format!("read config {path}"))?;
        let mut mode = cli.mode;
        let mut host = String::new();
        let mut port = 58321u16;
        let mut name = cli.name.clone();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "mode" => {
                    mode = if v.eq_ignore_ascii_case("inbound") {
                        Mode::Inbound
                    } else {
                        Mode::Outbound
                    }
                }
                "host" => host = v.into(),
                "port" => port = v.parse().unwrap_or(58321),
                "name" => name = v.into(),
                _ => {}
            }
        }
        let endpoint = match mode {
            Mode::Inbound => format!("0.0.0.0:{port}"),
            Mode::Outbound => format!(
                "{}:{port}",
                if host.is_empty() { "127.0.0.1" } else { &host }
            ),
        };
        return Ok(ResolvedConfig {
            mode,
            endpoint,
            name,
        });
    }
    Ok(ResolvedConfig {
        mode: cli.mode,
        endpoint: match cli.mode {
            Mode::Inbound => cli.listen.clone(),
            Mode::Outbound => cli.server.clone(),
        },
        name: cli.name.clone(),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = Cli::parse();
    let resolved = resolve_config(&cli)?;
    tracing::info!(mode = ?resolved.mode, name = %resolved.name, "SideWire configuration loaded");
    match resolved.mode {
        Mode::Inbound => run_inbound(&resolved.endpoint, &resolved.name).await,
        Mode::Outbound => run_outbound(&resolved.endpoint, &resolved.name).await,
    }
}
async fn run_inbound(bind: &str, name: &str) -> Result<()> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(%bind, "SideWire inbound daemon listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        stream.set_nodelay(true).context("enable TCP_NODELAY")?;
        let name = name.to_owned();
        tracing::info!(%peer, "host connected");
        tokio::spawn(async move {
            if let Err(error) = serve(stream, &name, true).await {
                tracing::warn!(%error, "connection ended");
            }
        });
    }
}

async fn run_outbound(server: &str, name: &str) -> Result<()> {
    let mut delay = 1u64;
    loop {
        match TcpStream::connect(server).await {
            Ok(stream) => {
                stream.set_nodelay(true).context("enable TCP_NODELAY")?;
                tracing::info!(%server, "connected to SideWire host");
                if let Err(error) = serve(stream, name, false).await {
                    tracing::warn!(%error, "host connection ended");
                }
                delay = 1;
            }
            Err(error) => tracing::warn!(%server, %error, "outbound connect failed"),
        }
        sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}
async fn serve(mut stream: TcpStream, name: &str, inbound: bool) -> Result<()> {
    let mut proxies: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    if inbound {
        let hello_frame = read_frame(&mut stream).await?;
        if hello_frame.kind != FrameKind::Hello {
            bail!("expected host Hello");
        }
        let hello: sidewire_protocol::Hello = decode(&hello_frame.payload)?;
        tracing::info!(peer = %hello.name, "handshake complete");
        send_ack(&mut stream, name).await?;
    } else {
        let hello = sidewire_protocol::Hello {
            name: name.to_owned(),
            role: sidewire_protocol::PeerRole::Device,
            protocol_version: sidewire_protocol::VERSION,
        };
        write_frame(&mut stream, &frame(FrameKind::Hello, 0, &hello)?).await?;
        let ack = read_frame(&mut stream).await?;
        if ack.kind != FrameKind::HelloAck {
            bail!("expected host HelloAck");
        }
    }

    loop {
        let request = read_frame(&mut stream).await?;
        match request.kind {
            FrameKind::ExecRequest => {
                handle_exec(&mut stream, request.stream_id, &request.payload).await?
            }
            FrameKind::PushRequest => {
                if let Err(error) =
                    handle_push(&mut stream, request.stream_id, &request.payload).await
                {
                    write_frame(
                        &mut stream,
                        &raw_frame(
                            FrameKind::Error,
                            request.stream_id,
                            error.to_string().into_bytes(),
                        ),
                    )
                    .await?;
                }
            }
            FrameKind::PullRequest => {
                if let Err(error) =
                    handle_pull(&mut stream, request.stream_id, &request.payload).await
                {
                    write_frame(
                        &mut stream,
                        &raw_frame(
                            FrameKind::Error,
                            request.stream_id,
                            error.to_string().into_bytes(),
                        ),
                    )
                    .await?;
                }
            }
            FrameKind::PtyOpen => {
                if let Err(error) =
                    handle_pty(&mut stream, request.stream_id, &request.payload).await
                {
                    write_frame(
                        &mut stream,
                        &raw_frame(
                            FrameKind::Error,
                            request.stream_id,
                            error.to_string().into_bytes(),
                        ),
                    )
                    .await?;
                }
            }
            FrameKind::ProxyStartRequest => {
                let req: ProxyStartRequest = decode(&request.payload)?;
                match start_proxy(&req).await {
                    Ok((ack, task)) => {
                        if let Some(old) = proxies.insert(req.id.clone(), task) {
                            old.abort();
                        }
                        write_frame(
                            &mut stream,
                            &frame(FrameKind::ProxyStartAck, request.stream_id, &ack)?,
                        )
                        .await?;
                    }
                    Err(error) => {
                        write_frame(
                            &mut stream,
                            &raw_frame(
                                FrameKind::Error,
                                request.stream_id,
                                error.to_string().into_bytes(),
                            ),
                        )
                        .await?
                    }
                }
            }
            FrameKind::Ping => {
                write_frame(
                    &mut stream,
                    &raw_frame(FrameKind::Pong, request.stream_id, request.payload),
                )
                .await?
            }
            kind => tracing::warn!(?kind, "ignoring unsupported request"),
        }
    }
}
async fn send_ack(stream: &mut TcpStream, name: &str) -> Result<()> {
    let ack = HelloAck {
        name: name.to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
    };
    write_frame(stream, &frame(FrameKind::HelloAck, 0, &ack)?).await
}

#[cfg(target_os = "android")]
fn apply_identity_now(identity: ExecIdentity) -> std::io::Result<()> {
    if matches!(identity, ExecIdentity::Root) {
        return Ok(());
    }
    const CONTEXT: &[u8] = b"u:r:shell:s0";
    const GROUPS: [libc::gid_t; 15] = [
        1004, 1007, 1011, 1015, 1028, 1078, 1079, 2000, 3001, 3002, 3003, 3006, 3009, 3011, 3012,
    ];
    let fd = unsafe {
        libc::open(
            c"/proc/self/attr/exec".as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let rc = unsafe { libc::write(fd, CONTEXT.as_ptr().cast(), CONTEXT.len()) };
    let saved = if rc < 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        libc::close(fd);
    }
    if let Some(e) = saved {
        return Err(e);
    }
    if unsafe { libc::setgroups(GROUPS.len(), GROUPS.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setgid(2000) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setuid(2000) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn apply_identity(command: &mut Command, identity: ExecIdentity) -> Result<()> {
    #[cfg(target_os = "android")]
    unsafe {
        command.pre_exec(move || apply_identity_now(identity));
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (command, identity);
    }
    Ok(())
}

async fn handle_exec(stream: &mut TcpStream, stream_id: u32, payload: &[u8]) -> Result<()> {
    let request: ExecRequest = decode(payload)?;
    let mut command = Command::new("/system/bin/sh");
    command
        .arg("-c")
        .arg("exec \"$0\" \"$@\"")
        .arg(&request.program)
        .args(&request.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_identity(&mut command, request.identity)?;
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .await
        .with_context(|| format!("execute {}", request.program))?;
    if !output.stdout.is_empty() {
        write_frame(
            stream,
            &raw_frame(FrameKind::ExecStdout, stream_id, output.stdout),
        )
        .await?;
    }
    if !output.stderr.is_empty() {
        write_frame(
            stream,
            &raw_frame(FrameKind::ExecStderr, stream_id, output.stderr),
        )
        .await?;
    }
    let exit = ExecExit {
        code: output.status.code(),
    };
    write_frame(stream, &frame(FrameKind::ExecExit, stream_id, &exit)?).await?;
    Ok(())
}

async fn identity_command(identity: ExecIdentity, script: &str, path: &str) -> Result<Command> {
    let mut command = Command::new("/system/bin/sh");
    command.arg("-c").arg(script).arg("sidewire-file").arg(path);
    apply_identity(&mut command, identity)?;
    Ok(command)
}

async fn file_size_for_identity(path: &str, identity: ExecIdentity) -> Result<u64> {
    let mut command =
        identity_command(identity, "exec /system/bin/stat -c %s -- \"$1\"", path).await?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .await
        .with_context(|| format!("stat {path}"))?;
    if !output.status.success() {
        bail!(
            "stat {}: {}",
            path,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .with_context(|| format!("parse size for {path}"))
}

async fn handle_push(stream: &mut TcpStream, stream_id: u32, payload: &[u8]) -> Result<()> {
    let request: FilePushRequest = decode(payload)?;
    let mut command = identity_command(
        request.identity,
        "exec /system/bin/cat > \"$1\"",
        &request.path,
    )
    .await?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("create {}", request.path))?;
    let mut child_stdin = child.stdin.take().context("push child stdin unavailable")?;
    write_frame(
        stream,
        &frame(FrameKind::FileMeta, stream_id, &FileMeta { size: 0 })?,
    )
    .await?;
    loop {
        let incoming = read_frame(stream).await?;
        if incoming.stream_id != stream_id {
            bail!("unexpected stream {} during push", incoming.stream_id);
        }
        match incoming.kind {
            FrameKind::FileChunk => child_stdin.write_all(&incoming.payload).await?,
            FrameKind::FileEnd => break,
            _ => bail!("unexpected frame {:?} during push", incoming.kind),
        }
    }
    child_stdin.shutdown().await?;
    drop(child_stdin);
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!(
            "write {}: {}",
            request.path,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    write_frame(
        stream,
        &raw_frame(FrameKind::FileEnd, stream_id, Vec::new()),
    )
    .await?;
    Ok(())
}

async fn handle_pull(stream: &mut TcpStream, stream_id: u32, payload: &[u8]) -> Result<()> {
    let request: FilePullRequest = decode(payload)?;
    let size = file_size_for_identity(&request.path, request.identity).await?;
    let mut command = identity_command(
        request.identity,
        "exec /system/bin/cat -- \"$1\"",
        &request.path,
    )
    .await?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("open {}", request.path))?;
    let mut stdout = child
        .stdout
        .take()
        .context("pull child stdout unavailable")?;
    write_frame(
        stream,
        &frame(FrameKind::FileMeta, stream_id, &FileMeta { size })?,
    )
    .await?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = stdout.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        write_frame(
            stream,
            &raw_frame(FrameKind::FileChunk, stream_id, buffer[..read].to_vec()),
        )
        .await?;
    }
    drop(stdout);
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!(
            "read {}: {}",
            request.path,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    write_frame(
        stream,
        &raw_frame(FrameKind::FileEnd, stream_id, Vec::new()),
    )
    .await?;
    Ok(())
}

#[cfg(target_os = "android")]
fn set_pty_size(fd: std::os::fd::RawFd, cols: u16, rows: u16) -> std::io::Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "android")]
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
    let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if master_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("posix_openpt");
    }
    if unsafe { libc::grantpt(master_fd) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(master_fd);
        }
        return Err(error).context("grantpt");
    }
    if unsafe { libc::unlockpt(master_fd) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(master_fd);
        }
        return Err(error).context("unlockpt");
    }
    let mut name = [0 as libc::c_char; 128];
    let rc = unsafe { libc::ptsname_r(master_fd, name.as_mut_ptr(), name.len()) };
    if rc != 0 {
        unsafe {
            libc::close(master_fd);
        }
        return Err(std::io::Error::from_raw_os_error(rc)).context("ptsname_r");
    }
    if let Err(error) = set_pty_size(master_fd, cols, rows) {
        unsafe {
            libc::close(master_fd);
        }
        return Err(error).context("set initial PTY size");
    }
    let write_fd = match dup_cloexec(master_fd) {
        Ok(fd) => fd,
        Err(error) => {
            unsafe {
                libc::close(master_fd);
            }
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

#[cfg(target_os = "android")]
fn configure_pty_child(
    slave_name: &std::ffi::CStr,
    identity: ExecIdentity,
    echo: bool,
) -> std::io::Result<()> {
    if unsafe { libc::setsid() } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let slave_fd = unsafe { libc::open(slave_name.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if slave_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(slave_fd, &mut attrs) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(slave_fd);
        }
        return Err(error);
    }
    if echo {
        attrs.c_lflag |= libc::ECHO;
    } else {
        attrs.c_lflag &= !(libc::ECHO | libc::ECHONL);
    }
    if unsafe { libc::tcsetattr(slave_fd, libc::TCSANOW, &attrs) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(slave_fd);
        }
        return Err(error);
    }
    if unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(slave_fd);
        }
        return Err(error);
    }
    for target in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        if unsafe { libc::dup2(slave_fd, target) } < 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(slave_fd);
            }
            return Err(error);
        }
    }
    if slave_fd > libc::STDERR_FILENO {
        unsafe {
            libc::close(slave_fd);
        }
    }
    apply_identity_now(identity)?;
    Ok(())
}

#[cfg(target_os = "android")]
fn terminate_pty_group(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGHUP);
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(target_os = "android")]
fn completion_token_start(line: &str, cursor: usize) -> usize {
    let mut start = 0usize;
    let mut quote = None;
    for (index, character) in line[..cursor].char_indices() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => {
                quote = Some(character);
                if start == index {
                    start = index + character.len_utf8();
                }
            }
            None if character.is_whitespace() || "|;&()<>".contains(character) => {
                start = index + character.len_utf8();
            }
            None => {}
        }
    }
    start
}

#[cfg(target_os = "android")]
fn common_completion_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = first.clone();
    for value in &values[1..] {
        while !value.starts_with(&prefix) {
            if prefix.pop().is_none() {
                break;
            }
        }
        if prefix.is_empty() {
            break;
        }
    }
    prefix
}

#[cfg(target_os = "android")]
fn complete_pty_path(pid: u32, request: &PtyCompleteRequest) -> Result<PtyCompleteResult> {
    let mut cursor = (request.cursor as usize).min(request.line.len());
    while cursor > 0 && !request.line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let start = completion_token_start(&request.line, cursor);
    let typed = &request.line[start..cursor];
    let split = typed.rfind('/').map(|index| index + 1).unwrap_or(0);
    let (directory_text, needle) = typed.split_at(split);

    let cwd = fs::read_link(format!("/proc/{pid}/cwd"))
        .with_context(|| format!("read PTY cwd for pid {pid}"))?;
    let search_dir = if directory_text.starts_with('/') {
        std::path::PathBuf::from(if directory_text.is_empty() {
            "/"
        } else {
            directory_text
        })
    } else if directory_text.is_empty() {
        cwd
    } else {
        cwd.join(directory_text)
    };

    let mut candidates = Vec::new();
    for entry in fs::read_dir(&search_dir)
        .with_context(|| format!("read completion directory {}", search_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(needle) {
            continue;
        }
        let mut candidate = format!("{directory_text}{name}");
        if entry
            .metadata()
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            candidate.push('/');
        }
        candidates.push(candidate);
    }
    candidates.sort();

    let replacement = match candidates.len() {
        0 => typed.to_owned(),
        1 => candidates.remove(0),
        _ => {
            let prefix = common_completion_prefix(&candidates);
            if prefix.len() > typed.len() {
                prefix
            } else {
                typed.to_owned()
            }
        }
    };

    let mut line = request.line.clone();
    line.replace_range(start..cursor, &replacement);
    let cursor = start + replacement.len();
    Ok(PtyCompleteResult {
        line,
        cursor: cursor as u32,
    })
}

async fn handle_pty(stream: &mut TcpStream, stream_id: u32, payload: &[u8]) -> Result<()> {
    let request: PtyOpenRequest = decode(payload)?;
    #[cfg(not(target_os = "android"))]
    {
        let _ = (stream, stream_id, request);
        bail!("PTY is only supported by the Android daemon");
    }
    #[cfg(target_os = "android")]
    {
        let hostname = android_shell_hostname();
        let (mut master_read, mut master_write, resize, slave_name) =
            open_pty_master(request.cols.max(1), request.rows.max(1))?;
        let mut command = Command::new(&request.program);
        command
            .args(&request.args)
            .env("TERM", &request.term)
            .env("COLORTERM", "truecolor")
            .env("HOSTNAME", hostname)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let identity = request.identity;
        let echo = request.echo;
        unsafe {
            command.pre_exec(move || configure_pty_child(&slave_name, identity, echo));
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn PTY program {}", request.program))?;
        let pid = child.id().unwrap_or(0);
        write_frame(
            stream,
            &frame(FrameKind::PtyOpenAck, stream_id, &PtyOpenAck { pid })?,
        )
        .await?;

        let (mut net_read, net_write) = tokio::io::split(stream);
        let net_write = tokio::sync::Mutex::new(net_write);
        let (done_tx, mut done_rx) = tokio::sync::watch::channel(false);

        let output_loop = async {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match master_read.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut writer = net_write.lock().await;
                        if let Err(error) = write_frame(
                            &mut *writer,
                            &raw_frame(FrameKind::PtyOutput, stream_id, buffer[..n].to_vec()),
                        )
                        .await
                        {
                            terminate_pty_group(pid);
                            return Err(error).context("write PTY output");
                        }
                    }
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) => {
                        let _ = done_tx.send(true);
                        return Err(error).context("read PTY master");
                    }
                }
            }
            let _ = done_tx.send(true);
            Ok::<(), anyhow::Error>(())
        };

        let input_loop = async {
            loop {
                tokio::select! {
                    _ = done_rx.changed() => return Ok::<(), anyhow::Error>(()),
                    incoming = read_frame(&mut net_read) => {
                        let incoming = match incoming {
                            Ok(frame) => frame,
                            Err(error) => {
                                terminate_pty_group(pid);
                                let _ = child.start_kill();
                                return Err(error);
                            }
                        };
                        if incoming.stream_id != stream_id {
                            terminate_pty_group(pid);
                                let _ = child.start_kill();
                            bail!("unexpected stream {} while PTY {stream_id} is active", incoming.stream_id);
                        }
                        match incoming.kind {
                            FrameKind::PtyInput => {
                                master_write.write_all(&incoming.payload).await?;
                                master_write.flush().await?;
                            }
                            FrameKind::PtyResize => {
                                let size: PtyResize = decode(&incoming.payload)?;
                                set_pty_size(resize.as_raw_fd(), size.cols.max(1), size.rows.max(1))?;
                            }
                            FrameKind::PtyComplete => {
                                let request: PtyCompleteRequest = decode(&incoming.payload)?;
                                let completion = complete_pty_path(pid, &request)?;
                                let mut writer = net_write.lock().await;
                                write_frame(
                                    &mut *writer,
                                    &frame(FrameKind::PtyCompleteResult, stream_id, &completion)?,
                                )
                                .await?;
                            }
                            FrameKind::PtyClose => {
                                terminate_pty_group(pid);
                                let _ = child.start_kill();
                                return Ok::<(), anyhow::Error>(());
                            }
                            kind => {
                                terminate_pty_group(pid);
                                let _ = child.start_kill();
                                bail!("unexpected PTY frame {kind:?}");
                            }
                        }
                    }
                }
            }
        };

        let (input_result, output_result) = tokio::join!(input_loop, output_loop);
        input_result?;
        output_result?;
        let status = child.wait().await?;
        let exit = PtyExit {
            code: status.code(),
        };
        let mut writer = net_write.lock().await;
        write_frame(&mut *writer, &frame(FrameKind::PtyExit, stream_id, &exit)?).await?;
        Ok(())
    }
}

#[cfg(target_os = "android")]
fn android_shell_hostname() -> String {
    let mut kernel_name = [0u8; 256];
    if unsafe {
        libc::gethostname(
            kernel_name.as_mut_ptr().cast::<libc::c_char>(),
            kernel_name.len(),
        )
    } == 0
    {
        let length = kernel_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(kernel_name.len());
        let value = String::from_utf8_lossy(&kernel_name[..length]);
        if !value.eq_ignore_ascii_case("localhost")
            && let Some(hostname) = sanitize_shell_hostname(&value)
        {
            return hostname;
        }
    }

    let mut property = [0 as libc::c_char; libc::PROP_VALUE_MAX as usize];
    let length = unsafe {
        libc::__system_property_get(c"ro.product.device".as_ptr(), property.as_mut_ptr())
    };
    if length > 0 {
        let bytes =
            unsafe { std::slice::from_raw_parts(property.as_ptr().cast::<u8>(), length as usize) };
        if let Some(hostname) = sanitize_shell_hostname(&String::from_utf8_lossy(bytes)) {
            return hostname;
        }
    }

    "android".to_owned()
}

#[cfg(any(target_os = "android", test))]
fn sanitize_shell_hostname(value: &str) -> Option<String> {
    let hostname: String = value
        .trim()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
        .collect();
    (!hostname.is_empty()).then_some(hostname)
}

async fn start_proxy(
    request: &ProxyStartRequest,
) -> Result<(ProxyStartAck, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(&request.bind)
        .await
        .with_context(|| format!("bind proxy {}", request.bind))?;
    let bound = listener.local_addr()?.to_string();
    let target = request.target.clone();
    let token = request.token.clone();
    let mode = request.token_mode;
    let id = request.id.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut incoming, peer)) = listener.accept().await else {
                break;
            };
            let target = target.clone();
            let token = token.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    match mode {
                        ProxyTokenMode::Expect => {
                            let mut received = vec![0u8; token.len()];
                            incoming.read_exact(&mut received).await?;
                            if received != token {
                                bail!("proxy token mismatch");
                            }
                            let mut outgoing = TcpStream::connect(&target).await?;
                            tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await?;
                        }
                        ProxyTokenMode::Send => {
                            let mut outgoing = TcpStream::connect(&target).await?;
                            outgoing.write_all(&token).await?;
                            outgoing.flush().await?;
                            tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await?;
                        }
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(%peer, %error, "proxy connection ended");
                }
            });
        }
    });
    Ok((ProxyStartAck { id, bound }, task))
}

#[cfg(test)]
mod tests {
    use super::sanitize_shell_hostname;

    #[test]
    fn shell_hostname_is_safe_for_an_android_prompt() {
        assert_eq!(sanitize_shell_hostname(" a53x\n"), Some("a53x".to_owned()));
        assert_eq!(
            sanitize_shell_hostname("SM-A536N_5G.test"),
            Some("SM-A536N_5G.test".to_owned())
        );
        assert_eq!(sanitize_shell_hostname(" \r\n\t"), None);
    }
}
