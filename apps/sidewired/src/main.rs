use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use sidewire_protocol::{
    ClipboardData, ClipboardSetRequest, DeviceId, ExecExit, ExecIdentity, ExecRequest, FileMeta,
    FilePullRequest, FilePushRequest, Frame, FrameKind, HelloAck, ProxyStartAck, ProxyStartRequest,
    ProxyTokenMode, PtyOpenRequest, SecureFrameReader, SecureFrameWriter, SecurityMode, decode,
    frame,
};
#[cfg(target_os = "android")]
use sidewire_protocol::{PtyCompleteRequest, PtyExit, PtyOpenAck, PtyResize};
#[cfg(target_os = "android")]
use std::fs::File as StdFile;
#[cfg(target_os = "android")]
use std::os::fd::{AsRawFd, FromRawFd};
use std::{collections::HashMap, fs, process::Stdio, sync::Arc};
mod completion;
mod security;
mod transport;

use transport::MuxWriter;

const FILE_BUFFER_SIZE: usize = 256 * 1024;
const PROXY_BUFFER_SIZE: usize = 64 * 1024;
const STREAM_ROUTE_CAPACITY: usize = 16;
const HOST_SILENCE_TIMEOUT_SECS: u64 = 45;
const STREAM_CANCEL_PROTOCOL_MINOR: u8 = 1;
const MAX_CLIPBOARD_TEXT: usize = 4 * 1024 * 1024;

type StreamRoutes = Arc<tokio::sync::Mutex<HashMap<u32, tokio::sync::mpsc::Sender<Frame>>>>;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    process::Command,
    sync::Notify,
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
    device_id: Option<String>,
    /// Disable authentication and encryption for direct development runs.
    #[arg(long)]
    insecure: bool,
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
    device_id: DeviceId,
    security: SecurityMode,
    pairing_file: Option<String>,
    pairs_dir: Option<String>,
    pairing_port: u16,
    clipboard_helper: Option<String>,
}

fn resolve_config(cli: &Cli) -> Result<ResolvedConfig> {
    let mut mode = cli.mode;
    let mut host = String::new();
    let mut port = 58321u16;
    let mut name = cli.name.clone();
    let mut device_id = cli.device_id.as_deref().map(DeviceId::parse).transpose()?;
    let mut security = if cli.insecure {
        SecurityMode::Insecure
    } else {
        SecurityMode::Secure
    };
    let mut pairing_file = None;
    let mut pairs_dir = None;
    let mut pairing_port = sidewire_protocol::PAIRING_PORT;
    let mut clipboard_helper = None;
    if let Some(path) = &cli.config {
        let text = fs::read_to_string(path).with_context(|| format!("read config {path}"))?;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "mode" => {
                    mode = if value.eq_ignore_ascii_case("inbound") {
                        Mode::Inbound
                    } else {
                        Mode::Outbound
                    }
                }
                "host" => host = value.into(),
                "port" => port = value.parse().unwrap_or(58321),
                "name" => name = value.into(),
                "device_id" if !value.is_empty() => device_id = Some(DeviceId::parse(value)?),
                "security" => {
                    security = if value.eq_ignore_ascii_case("insecure") {
                        SecurityMode::Insecure
                    } else {
                        SecurityMode::Secure
                    }
                }
                "pairing_file" if !value.is_empty() => pairing_file = Some(value.into()),
                "pairs_dir" if !value.is_empty() => pairs_dir = Some(value.into()),
                "pairing_port" => {
                    pairing_port = value.parse().unwrap_or(sidewire_protocol::PAIRING_PORT)
                }
                "clipboard_helper" if !value.is_empty() => clipboard_helper = Some(value.into()),
                _ => {}
            }
        }
    }
    let endpoint = if cli.config.is_some() {
        match mode {
            Mode::Inbound => format!("0.0.0.0:{port}"),
            Mode::Outbound => format!(
                "{}:{port}",
                if host.is_empty() { "127.0.0.1" } else { &host }
            ),
        }
    } else {
        match mode {
            Mode::Inbound => cli.listen.clone(),
            Mode::Outbound => cli.server.clone(),
        }
    };
    let device_id = device_id.unwrap_or_else(|| DeviceId(rand::random::<[u8; 16]>()));
    Ok(ResolvedConfig {
        mode,
        endpoint,
        name,
        device_id,
        security,
        pairing_file,
        pairs_dir,
        pairing_port,
        clipboard_helper,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_ansi(false)
        .init();
    let cli = Cli::parse();
    let resolved = resolve_config(&cli)?;
    if resolved.security == SecurityMode::Insecure {
        tracing::warn!(
            "INSECURE MODE: SideWire authentication and traffic encryption are disabled"
        );
    }
    tracing::info!(
        mode = ?resolved.mode,
        name = %resolved.name,
        device_id = %resolved.device_id,
        security = resolved.security.as_str(),
        "SideWire configuration loaded"
    );
    let reconnect = Arc::new(Notify::new());
    let pairing = match (resolved.pairing_file.clone(), resolved.pairs_dir.clone()) {
        (Some(pairing_file), Some(pairs_dir)) => Some(security::run_pairing_listener(
            resolved.pairing_port,
            resolved.name.clone(),
            resolved.device_id,
            pairing_file,
            pairs_dir,
            Some(reconnect.clone()),
        )),
        _ => None,
    };
    let device = async {
        match resolved.mode {
            Mode::Inbound => {
                run_inbound(
                    &resolved.endpoint,
                    &resolved.name,
                    resolved.device_id,
                    resolved.security,
                    resolved.pairs_dir.clone(),
                    resolved.clipboard_helper.clone(),
                )
                .await
            }
            Mode::Outbound => {
                run_outbound(
                    &resolved.endpoint,
                    &resolved.name,
                    resolved.device_id,
                    resolved.security,
                    resolved.pairs_dir.clone(),
                    resolved.clipboard_helper.clone(),
                    reconnect.clone(),
                )
                .await
            }
        }
    };
    match pairing {
        Some(pairing) => tokio::select! {
            result = pairing => result,
            result = device => result,
        },
        None => device.await,
    }
}

async fn run_inbound(
    bind: &str,
    name: &str,
    device_id: DeviceId,
    security: SecurityMode,
    pairs_dir: Option<String>,
    clipboard_helper: Option<String>,
) -> Result<()> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let listen_port = listener.local_addr()?.port();
    let discovery_name = name.to_owned();
    tokio::spawn(async move {
        if let Err(error) =
            run_discovery_responder(discovery_name, device_id, listen_port, security).await
        {
            tracing::warn!(%error, "SideWire discovery responder stopped");
        }
    });
    tracing::info!(%bind, "SideWire inbound daemon listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        stream.set_nodelay(true).context("enable TCP_NODELAY")?;
        let name = name.to_owned();
        let pairs_dir = pairs_dir.clone();
        let clipboard_helper = clipboard_helper.clone();
        tracing::info!(%peer, "host connected");
        tokio::spawn(async move {
            if let Err(error) = serve(
                stream,
                &name,
                device_id,
                true,
                security,
                pairs_dir,
                clipboard_helper,
            )
            .await
            {
                tracing::warn!(%error, "connection ended");
            }
        });
    }
}

async fn run_discovery_responder(
    name: String,
    device_id: DeviceId,
    listen_port: u16,
    security: SecurityMode,
) -> Result<()> {
    let socket = UdpSocket::bind(("0.0.0.0", sidewire_protocol::DISCOVERY_PORT))
        .await
        .context("bind SideWire discovery responder")?;
    let reply = sidewire_protocol::DiscoveryReply {
        device_id,
        name,
        port: listen_port,
        protocol_version: sidewire_protocol::VERSION,
        security,
    };
    let encoded = sidewire_protocol::encode(&reply)?;
    let mut buffer = [0u8; 256];
    loop {
        let (size, peer) = socket.recv_from(&mut buffer).await?;
        if &buffer[..size] != sidewire_protocol::DISCOVERY_REQUEST {
            continue;
        }
        socket.send_to(&encoded, peer).await?;
    }
}

async fn run_outbound(
    server: &str,
    name: &str,
    device_id: DeviceId,
    security: SecurityMode,
    pairs_dir: Option<String>,
    clipboard_helper: Option<String>,
    reconnect: Arc<Notify>,
) -> Result<()> {
    let mut delay = 1u64;
    let mut pairing_grace = 0u8;
    loop {
        match TcpStream::connect(server).await {
            Ok(stream) => {
                stream.set_nodelay(true).context("enable TCP_NODELAY")?;
                tracing::info!(%server, "connected to SideWire host");
                if let Err(error) = serve(
                    stream,
                    name,
                    device_id,
                    false,
                    security,
                    pairs_dir.clone(),
                    clipboard_helper.clone(),
                )
                .await
                {
                    tracing::warn!(%error, "host connection ended");
                }
                delay = 1;
            }
            Err(error) => tracing::warn!(%server, %error, "outbound connect failed"),
        }
        tokio::select! {
            _ = sleep(Duration::from_secs(delay)) => {
                if pairing_grace > 0 {
                    pairing_grace -= 1;
                    delay = 1;
                } else {
                    delay = (delay * 2).min(5);
                }
            }
            _ = reconnect.notified() => {
                tracing::info!(%server, "pairing completed; enabling rapid outbound reconnects");
                pairing_grace = 60;
                delay = 1;
            }
        }
    }
}

async fn register_stream(
    routes: &StreamRoutes,
    stream_id: u32,
) -> Result<tokio::sync::mpsc::Receiver<Frame>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(STREAM_ROUTE_CAPACITY);
    let mut routes = routes.lock().await;
    if routes.contains_key(&stream_id) {
        bail!("stream {stream_id} is already active");
    }
    routes.insert(stream_id, sender);
    Ok(receiver)
}

async fn unregister_stream(routes: &StreamRoutes, stream_id: u32) {
    routes.lock().await.remove(&stream_id);
}

fn daemon_capabilities(clipboard_helper: Option<&str>) -> u64 {
    let mut capabilities = sidewire_protocol::capabilities::CORE
        | sidewire_protocol::capabilities::PTY_COMPLETION
        | sidewire_protocol::capabilities::SECURE_PROXY
        | sidewire_protocol::capabilities::HEARTBEAT
        | sidewire_protocol::capabilities::STREAM_CANCEL;
    if clipboard_helper.is_some_and(|path| std::path::Path::new(path).is_file()) {
        capabilities |= sidewire_protocol::capabilities::CLIPBOARD;
    }
    capabilities
}

async fn serve(
    mut stream: TcpStream,
    name: &str,
    device_id: DeviceId,
    inbound: bool,
    security_mode: SecurityMode,
    pairs_dir: Option<String>,
    clipboard_helper: Option<String>,
) -> Result<()> {
    let local_capabilities = daemon_capabilities(clipboard_helper.as_deref());
    let secured = if inbound {
        security::accept_connection(&mut stream, device_id, security_mode, pairs_dir.as_deref())
            .await?
    } else {
        security::connect_connection(&mut stream, device_id, security_mode, pairs_dir.as_deref())
            .await?
    };
    let noise = secured.noise.clone();
    let shared_secret = secured.shared_secret;
    let (peer_capabilities, negotiated_protocol) = if inbound {
        let hello_frame = {
            let mut reader = SecureFrameReader::new(&mut stream, noise.clone());
            reader.read_frame().await?
        };
        if hello_frame.kind != FrameKind::Hello {
            bail!("expected host Hello");
        }
        let hello: sidewire_protocol::Hello = decode(&hello_frame.payload)?;
        let protocol = sidewire_protocol::negotiated_version(hello.protocol_version)
            .context("host protocol major is incompatible")?;
        if hello.capabilities & sidewire_protocol::capabilities::CORE == 0 {
            bail!("host does not advertise the core capability");
        }
        tracing::info!(peer = %hello.name, host_id = %secured.peer_id.short(), security = security_mode.as_str(), "handshake complete");
        send_ack(
            &mut stream,
            name,
            device_id,
            noise.clone(),
            local_capabilities,
        )
        .await?;
        (hello.capabilities, protocol)
    } else {
        let hello = sidewire_protocol::Hello {
            device_id: Some(device_id),
            name: name.to_owned(),
            role: sidewire_protocol::PeerRole::Device,
            protocol_version: sidewire_protocol::VERSION,
            capabilities: local_capabilities,
        };
        {
            let mut writer = SecureFrameWriter::new(&mut stream, noise.clone());
            writer
                .write_frame(&frame(FrameKind::Hello, 0, &hello)?)
                .await?;
        }
        let ack = {
            let mut reader = SecureFrameReader::new(&mut stream, noise.clone());
            reader.read_frame().await?
        };
        if ack.kind != FrameKind::HelloAck {
            bail!("expected host HelloAck");
        }
        let ack: HelloAck = decode(&ack.payload)?;
        let protocol = sidewire_protocol::negotiated_version(ack.protocol_version)
            .context("host protocol major is incompatible")?;
        if ack.capabilities & sidewire_protocol::capabilities::CORE == 0 {
            bail!("host does not advertise the core capability");
        }
        (ack.capabilities, protocol)
    };
    let negotiated_capabilities = local_capabilities & peer_capabilities;
    let supports_stream_cancel = sidewire_protocol::protocol_minor(negotiated_protocol)
        >= STREAM_CANCEL_PROTOCOL_MINOR
        && negotiated_capabilities & sidewire_protocol::capabilities::STREAM_CANCEL != 0;

    let (reader_half, writer_half) = stream.into_split();
    let mut reader = SecureFrameReader::new(reader_half, noise.clone());
    let writer = MuxWriter::new(writer_half, noise, supports_stream_cancel);
    let routes: StreamRoutes = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let proxies = Arc::new(tokio::sync::Mutex::new(HashMap::<
        String,
        tokio::task::JoinHandle<()>,
    >::new()));

    let heartbeat = negotiated_capabilities & sidewire_protocol::capabilities::HEARTBEAT != 0;
    let result: Result<()> = async {
        loop {
            let request = if heartbeat {
                match tokio::time::timeout(
                    Duration::from_secs(HOST_SILENCE_TIMEOUT_SECS),
                    reader.read_frame(),
                )
                .await
                {
                    Ok(result) => result?,
                    Err(_) => bail!("host heartbeat timeout"),
                }
            } else {
                reader.read_frame().await?
            };
            let stream_id = request.stream_id;
            if request.kind == FrameKind::StreamCancel {
                writer.cancel_local(stream_id);
                let removed = routes.lock().await.remove(&stream_id).is_some();
                tracing::debug!(stream_id, routed = removed, "host canceled device stream");
                continue;
            }

            let routed = { routes.lock().await.get(&stream_id).cloned() };
            if let Some(sender) = routed {
                match sender.try_send(request) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        routes.lock().await.remove(&stream_id);
                        let reason =
                            format!("stream {stream_id} receive queue overflow; stream canceled");
                        tracing::warn!(stream_id, "host stream receive queue overflow");
                        let writer = writer.clone();
                        tokio::spawn(async move {
                            writer.send_cancel(stream_id, reason).await;
                        });
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        routes.lock().await.remove(&stream_id);
                    }
                }
                continue;
            }

            match request.kind {
                FrameKind::ExecRequest => {
                    let writer = writer.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_exec(&writer, stream_id, &request.payload).await
                        {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::PushRequest => {
                    let mut inbox = register_stream(&routes, stream_id).await?;
                    let writer = writer.clone();
                    let routes = routes.clone();
                    tokio::spawn(async move {
                        let result =
                            handle_push(&writer, stream_id, &request.payload, &mut inbox).await;
                        unregister_stream(&routes, stream_id).await;
                        if let Err(error) = result {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::PullRequest => {
                    let writer = writer.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_pull(&writer, stream_id, &request.payload).await
                        {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::PtyOpen => {
                    let mut inbox = register_stream(&routes, stream_id).await?;
                    let writer = writer.clone();
                    let routes = routes.clone();
                    let device_name = name.to_owned();
                    tokio::spawn(async move {
                        let result = handle_pty(
                            &writer,
                            stream_id,
                            &request.payload,
                            &mut inbox,
                            &device_name,
                        )
                        .await;
                        unregister_stream(&routes, stream_id).await;
                        if let Err(error) = result {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::ClipboardGet => {
                    let writer = writer.clone();
                    let helper = clipboard_helper.clone();
                    tokio::spawn(async move {
                        let result: Result<()> = async {
                            let helper =
                                helper.context("clipboard helper is unavailable on this device")?;
                            let text = run_clipboard_helper(&helper, "get", None).await?;
                            writer
                                .send(&frame(
                                    FrameKind::ClipboardData,
                                    stream_id,
                                    &ClipboardData { text: Some(text) },
                                )?)
                                .await
                        }
                        .await;
                        if let Err(error) = result {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::ClipboardSet => {
                    let writer = writer.clone();
                    let helper = clipboard_helper.clone();
                    tokio::spawn(async move {
                        let result: Result<()> = async {
                            let helper =
                                helper.context("clipboard helper is unavailable on this device")?;
                            let request: ClipboardSetRequest = decode(&request.payload)?;
                            if request.text.len() > MAX_CLIPBOARD_TEXT {
                                bail!("clipboard text exceeds 4 MiB limit");
                            }
                            run_clipboard_helper(&helper, "set", Some(&request.text)).await?;
                            writer
                                .send(&frame(
                                    FrameKind::ClipboardData,
                                    stream_id,
                                    &ClipboardData { text: None },
                                )?)
                                .await
                        }
                        .await;
                        if let Err(error) = result {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::ClipboardClear => {
                    let writer = writer.clone();
                    let helper = clipboard_helper.clone();
                    tokio::spawn(async move {
                        let result: Result<()> = async {
                            let helper =
                                helper.context("clipboard helper is unavailable on this device")?;
                            run_clipboard_helper(&helper, "clear", None).await?;
                            writer
                                .send(&frame(
                                    FrameKind::ClipboardData,
                                    stream_id,
                                    &ClipboardData { text: None },
                                )?)
                                .await
                        }
                        .await;
                        if let Err(error) = result {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::ProxyStartRequest => {
                    let writer = writer.clone();
                    let proxies = proxies.clone();
                    tokio::spawn(async move {
                        let result: Result<()> = async {
                            let req: ProxyStartRequest = decode(&request.payload)?;
                            let (ack, task) = start_proxy(&req, shared_secret).await?;
                            if let Some(old) = proxies.lock().await.insert(req.id.clone(), task) {
                                old.abort();
                            }
                            writer
                                .send(&frame(FrameKind::ProxyStartAck, stream_id, &ack)?)
                                .await
                        }
                        .await;
                        if let Err(error) = result {
                            writer.send_error(stream_id, error).await;
                        }
                    });
                }
                FrameKind::Ping => {
                    writer
                        .send_raw(FrameKind::Pong, stream_id, &request.payload)
                        .await?;
                }
                kind => {
                    writer
                        .send_error(stream_id, format!("unsupported request {kind:?}"))
                        .await;
                }
            }
        }
    }
    .await;

    routes.lock().await.clear();
    for (_, task) in proxies.lock().await.drain() {
        task.abort();
    }
    result
}
async fn send_ack(
    stream: &mut TcpStream,
    name: &str,
    device_id: DeviceId,
    noise: Option<sidewire_protocol::SharedNoise>,
    capabilities: u64,
) -> Result<()> {
    let ack = HelloAck {
        device_id: Some(device_id),
        name: name.to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        protocol_version: sidewire_protocol::VERSION,
        capabilities,
    };
    let mut writer = SecureFrameWriter::new(stream, noise);
    writer
        .write_frame(&frame(FrameKind::HelloAck, 0, &ack)?)
        .await
}

async fn run_clipboard_helper(
    helper: &str,
    operation: &str,
    input: Option<&str>,
) -> Result<String> {
    if !std::path::Path::new(helper).is_file() {
        bail!("clipboard helper not found at {helper}");
    }
    let args = vec![
        "/system/bin".to_owned(),
        "com.sidewire.ClipboardHelper".to_owned(),
        operation.to_owned(),
    ];
    let mut command = command_for_identity("/system/bin/app_process", &args, ExecIdentity::Shell);
    command
        .env("CLASSPATH", helper)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("start Android clipboard helper")?;
    if let Some(text) = input {
        let mut stdin = child.stdin.take().context("open clipboard helper stdin")?;
        stdin.write_all(text.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!(
            "Android clipboard helper failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if output.stdout.len() > MAX_CLIPBOARD_TEXT {
        bail!("Android clipboard text exceeds 4 MiB limit");
    }
    String::from_utf8(output.stdout).context("Android clipboard is not valid UTF-8")
}

#[cfg(target_os = "android")]
const SHELL_GROUPS: &[u32] = &[
    1004, 1007, 1011, 1015, 1028, 1078, 1079, 2000, 3001, 3002, 3003, 3006, 3009, 3011, 3012,
];

#[cfg(any(target_os = "android", test))]
fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn command_for_identity(program: &str, args: &[String], identity: ExecIdentity) -> Command {
    #[cfg(not(target_os = "android"))]
    let _ = identity;
    #[cfg(target_os = "android")]
    if matches!(identity, ExecIdentity::Shell) {
        let mut command = Command::new("/system/bin/su");
        command
            .arg("-p")
            .arg("-Z")
            .arg("u:r:shell:s0")
            .arg("-g")
            .arg("2000");
        for group in SHELL_GROUPS {
            command.arg("-G").arg(group.to_string());
        }
        command.arg("--ksu-no-new-privs").arg("2000").arg("-c");
        let mut script = String::from("exec ");
        script.push_str(&shell_quote(program));
        for arg in args {
            script.push(' ');
            script.push_str(&shell_quote(arg));
        }
        command.arg(script);
        return command;
    }

    let mut command = Command::new(program);
    command.args(args);
    command
}

async fn handle_exec(writer: &MuxWriter, stream_id: u32, payload: &[u8]) -> Result<()> {
    let request: ExecRequest = decode(payload)?;
    let mut command = command_for_identity(&request.program, &request.args, request.identity);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("execute {}", request.program))?;
    let mut stdout = child.stdout.take().context("exec stdout unavailable")?;
    let mut stderr = child.stderr.take().context("exec stderr unavailable")?;
    let mut stdout_buffer = vec![0u8; 64 * 1024];
    let mut stderr_buffer = vec![0u8; 64 * 1024];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut status = None;
    let wait = child.wait();
    tokio::pin!(wait);

    while !stdout_done || !stderr_done || status.is_none() {
        tokio::select! {
            read = stdout.read(&mut stdout_buffer), if !stdout_done => {
                let read = read?;
                if read == 0 {
                    stdout_done = true;
                } else {
                    writer
                        .send_raw(FrameKind::ExecStdout, stream_id, &stdout_buffer[..read])
                        .await?;
                }
            }
            read = stderr.read(&mut stderr_buffer), if !stderr_done => {
                let read = read?;
                if read == 0 {
                    stderr_done = true;
                } else {
                    writer
                        .send_raw(FrameKind::ExecStderr, stream_id, &stderr_buffer[..read])
                        .await?;
                }
            }
            result = &mut wait, if status.is_none() => {
                status = Some(result?);
            }
        }
    }

    let exit = ExecExit {
        code: status.and_then(|status| status.code()),
    };
    writer
        .send(&frame(FrameKind::ExecExit, stream_id, &exit)?)
        .await?;
    Ok(())
}

async fn identity_command(identity: ExecIdentity, script: &str, path: &str) -> Result<Command> {
    let args = vec![
        "-c".to_owned(),
        script.to_owned(),
        "sidewire-file".to_owned(),
        path.to_owned(),
    ];
    Ok(command_for_identity("/system/bin/sh", &args, identity))
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

async fn handle_push(
    writer: &MuxWriter,
    stream_id: u32,
    payload: &[u8],
    inbox: &mut tokio::sync::mpsc::Receiver<Frame>,
) -> Result<()> {
    let request: FilePushRequest = decode(payload)?;
    if matches!(request.identity, ExecIdentity::Root) {
        let mut file = tokio::fs::File::create(&request.path)
            .await
            .with_context(|| format!("create {}", request.path))?;
        writer
            .send(&frame(
                FrameKind::FileMeta,
                stream_id,
                &FileMeta { size: 0 },
            )?)
            .await?;
        loop {
            let incoming = inbox
                .recv()
                .await
                .context("device connection closed during push")?;
            if incoming.stream_id != stream_id {
                bail!("unexpected stream {} during push", incoming.stream_id);
            }
            match incoming.kind {
                FrameKind::FileChunk => file.write_all(&incoming.payload).await?,
                FrameKind::FileEnd => break,
                _ => bail!("unexpected frame {:?} during push", incoming.kind),
            }
        }
        file.flush().await?;
        writer.send_raw(FrameKind::FileEnd, stream_id, &[]).await?;
        return Ok(());
    }

    let mut command = identity_command(
        request.identity,
        "exec 3>\"$1\" || exit $?; printf R; exec /system/bin/cat >&3",
        &request.path,
    )
    .await?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("create {}", request.path))?;
    let mut child_stdin = child.stdin.take().context("push child stdin unavailable")?;
    let mut child_stdout = child
        .stdout
        .take()
        .context("push child stdout unavailable")?;
    let mut ready = [0u8; 1];
    let readiness = child_stdout.read_exact(&mut ready).await;
    drop(child_stdout);
    if readiness.is_err() || ready[0] != b'R' {
        drop(child_stdin);
        let output = child.wait_with_output().await?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if !stderr.is_empty() {
            bail!("write {}: {stderr}", request.path);
        }
        readiness.with_context(|| format!("open {} for push", request.path))?;
        bail!("open {} for push did not become ready", request.path);
    }
    writer
        .send(&frame(
            FrameKind::FileMeta,
            stream_id,
            &FileMeta { size: 0 },
        )?)
        .await?;

    let transfer_result: Result<()> = async {
        loop {
            let incoming = inbox
                .recv()
                .await
                .context("device connection closed during push")?;
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
        Ok(())
    }
    .await;
    drop(child_stdin);
    let output = child.wait_with_output().await?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if let Err(error) = transfer_result {
        if !stderr.is_empty() {
            bail!("write {}: {stderr}", request.path);
        }
        return Err(error).with_context(|| format!("write {}", request.path));
    }
    if !output.status.success() {
        bail!("write {}: {stderr}", request.path);
    }
    writer.send_raw(FrameKind::FileEnd, stream_id, &[]).await?;
    Ok(())
}

async fn handle_pull(writer: &MuxWriter, stream_id: u32, payload: &[u8]) -> Result<()> {
    let request: FilePullRequest = decode(payload)?;
    if matches!(request.identity, ExecIdentity::Root) {
        let mut file = tokio::fs::File::open(&request.path)
            .await
            .with_context(|| format!("open {}", request.path))?;
        let size = file.metadata().await?.len();
        writer
            .send(&frame(FrameKind::FileMeta, stream_id, &FileMeta { size })?)
            .await?;
        let mut buffer = vec![0u8; FILE_BUFFER_SIZE];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            writer
                .send_raw(FrameKind::FileChunk, stream_id, &buffer[..read])
                .await?;
        }
        writer.send_raw(FrameKind::FileEnd, stream_id, &[]).await?;
        return Ok(());
    }

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
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("open {}", request.path))?;
    let mut stdout = child
        .stdout
        .take()
        .context("pull child stdout unavailable")?;
    writer
        .send(&frame(FrameKind::FileMeta, stream_id, &FileMeta { size })?)
        .await?;
    let mut buffer = vec![0u8; FILE_BUFFER_SIZE];
    loop {
        let read = stdout.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        writer
            .send_raw(FrameKind::FileChunk, stream_id, &buffer[..read])
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
    writer.send_raw(FrameKind::FileEnd, stream_id, &[]).await?;
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
fn configure_pty_child(slave_name: &std::ffi::CStr, echo: bool) -> std::io::Result<()> {
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

async fn handle_pty(
    writer: &MuxWriter,
    stream_id: u32,
    payload: &[u8],
    inbox: &mut tokio::sync::mpsc::Receiver<Frame>,
    device_name: &str,
) -> Result<()> {
    let request: PtyOpenRequest = decode(payload)?;
    #[cfg(not(target_os = "android"))]
    {
        let _ = (writer, stream_id, request, inbox, device_name);
        bail!("PTY is only supported by the Android daemon");
    }
    #[cfg(target_os = "android")]
    {
        let hostname = sanitize_shell_hostname(device_name).unwrap_or_else(android_shell_hostname);
        let (mut master_read, mut master_write, resize, slave_name) =
            open_pty_master(request.cols.max(1), request.rows.max(1))?;
        let mut command = command_for_identity(&request.program, &request.args, request.identity);
        command
            .env("TERM", &request.term)
            .env("COLORTERM", "truecolor")
            .env("HOSTNAME", hostname)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let echo = request.echo;
        unsafe {
            command.pre_exec(move || configure_pty_child(&slave_name, echo));
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn PTY program {}", request.program))?;
        let pid = child.id().unwrap_or(0);
        writer
            .send(&frame(
                FrameKind::PtyOpenAck,
                stream_id,
                &PtyOpenAck { pid },
            )?)
            .await?;

        let (done_tx, mut done_rx) = tokio::sync::watch::channel(false);

        let output_loop = async {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match master_read.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Err(error) = writer
                            .send_raw(FrameKind::PtyOutput, stream_id, &buffer[..n])
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
                    incoming = inbox.recv() => {
                        let Some(incoming) = incoming else {
                            terminate_pty_group(pid);
                            let _ = child.start_kill();
                            bail!("device connection closed while PTY {stream_id} was active");
                        };
                        if incoming.stream_id != stream_id {
                            terminate_pty_group(pid);
                                let _ = child.start_kill();
                            bail!("unexpected stream {} while PTY {stream_id} is active", incoming.stream_id);
                        }
                        match incoming.kind {
                            FrameKind::PtyInput => {
                                master_write.write_all(&incoming.payload).await?;
                            }
                            FrameKind::PtyResize => {
                                let size: PtyResize = decode(&incoming.payload)?;
                                set_pty_size(resize.as_raw_fd(), size.cols.max(1), size.rows.max(1))?;
                            }
                            FrameKind::PtyComplete => {
                                let request: PtyCompleteRequest = decode(&incoming.payload)?;
                                let completion = tokio::task::spawn_blocking(move || {
                                    completion::complete_pty_path(pid, &request)
                                })
                                .await
                                .context("join PTY completion worker")??;
                                writer
                                    .send(&frame(
                                        FrameKind::PtyCompleteResult,
                                        stream_id,
                                        &completion,
                                    )?)
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
        writer
            .send(&frame(FrameKind::PtyExit, stream_id, &exit)?)
            .await?;
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
    shared_secret: Option<[u8; 32]>,
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
            let _ = incoming.set_nodelay(true);
            let target = target.clone();
            let token = token.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    match mode {
                        ProxyTokenMode::Expect => {
                            let mut outgoing = TcpStream::connect(&target).await?;
                            outgoing.set_nodelay(true)?;
                            if let Some(secret) = shared_secret {
                                let prologue = sidewire_protocol::proxy_prologue(&token, "forward");
                                let noise = sidewire_protocol::noise_responder(
                                    &mut incoming,
                                    &secret,
                                    &prologue,
                                )
                                .await?;
                                sidewire_protocol::copy_noise_tunnel(
                                    &mut outgoing,
                                    &mut incoming,
                                    noise,
                                )
                                .await?;
                            } else {
                                let mut received = vec![0u8; token.len()];
                                incoming.read_exact(&mut received).await?;
                                if received != token {
                                    bail!("proxy token mismatch");
                                }
                                tokio::io::copy_bidirectional_with_sizes(
                                    &mut incoming,
                                    &mut outgoing,
                                    PROXY_BUFFER_SIZE,
                                    PROXY_BUFFER_SIZE,
                                )
                                .await?;
                            }
                        }
                        ProxyTokenMode::Send => {
                            let mut outgoing = TcpStream::connect(&target).await?;
                            outgoing.set_nodelay(true)?;
                            if let Some(secret) = shared_secret {
                                let prologue = sidewire_protocol::proxy_prologue(&token, "reverse");
                                let noise = sidewire_protocol::noise_initiator(
                                    &mut outgoing,
                                    &secret,
                                    &prologue,
                                )
                                .await?;
                                sidewire_protocol::copy_noise_tunnel(
                                    &mut incoming,
                                    &mut outgoing,
                                    noise,
                                )
                                .await?;
                            } else {
                                outgoing.write_all(&token).await?;
                                tokio::io::copy_bidirectional_with_sizes(
                                    &mut incoming,
                                    &mut outgoing,
                                    PROXY_BUFFER_SIZE,
                                    PROXY_BUFFER_SIZE,
                                )
                                .await?;
                            }
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
    use super::{sanitize_shell_hostname, shell_quote};

    #[test]
    fn shell_quote_preserves_arguments() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("한글 $HOME ; x"), "'한글 $HOME ; x'");
    }
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
