use super::*;
use crate::transport::DeviceTransport;
use sidewire_protocol::{
    DeviceId, SecureFrameReader, SecureFrameWriter, SecurityBanner, SecurityClientHello,
    SecurityDecision, SecurityMode, SharedNoise, negotiated_version, noise_initiator,
    noise_responder, protocol_compatible, protocol_label, read_packet, security_prologue,
    write_packet,
};
use std::collections::HashSet;

const FILE_BUFFER_SIZE: usize = 256 * 1024;
const PROXY_BUFFER_SIZE: usize = 64 * 1024;
const HEARTBEAT_INTERVAL_SECS: u64 = 10;
const HEARTBEAT_TIMEOUT_SECS: u64 = 10;
const HEARTBEAT_MAX_MISSES: u32 = 3;

#[derive(Clone, Copy, Debug)]
enum ConnectionMode {
    Inbound,
    Outbound,
}

impl ConnectionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Clone)]
struct DeviceSession {
    id: DeviceId,
    name: String,
    pub(super) peer: String,
    mode: ConnectionMode,
    device_ip: IpAddr,
    local_ip: IpAddr,
    transport: DeviceTransport,
    security: SecurityMode,
    shared_secret: Option<[u8; 32]>,
    capabilities: u64,
    protocol: u16,
}

type DeviceMap = Arc<RwLock<HashMap<DeviceId, DeviceSession>>>;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DeviceInfo {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) peer: String,
    pub(super) mode: String,
    pub(super) security: String,
    pub(super) protocol: String,
    pub(super) capabilities: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum ControlRequest {
    Devices,
    Ping {
        device: Option<String>,
    },
    Exec {
        device: Option<String>,
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        run_as: RunAs,
    },
    ExecStream {
        device: Option<String>,
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        run_as: RunAs,
    },
    Push {
        device: Option<String>,
        local: String,
        remote: String,
        run_as: RunAs,
    },
    Pull {
        device: Option<String>,
        remote: String,
        local: String,
        run_as: RunAs,
    },
    Pty {
        device: Option<String>,
        program: String,
        args: Vec<String>,
        run_as: RunAs,
        cols: u16,
        rows: u16,
        term: String,
        echo: bool,
    },
    ClipboardGet {
        device: Option<String>,
    },
    ClipboardSet {
        device: Option<String>,
        text: String,
    },
    ClipboardClear {
        device: Option<String>,
    },
    Forward {
        device: Option<String>,
        local_port: u16,
        remote_port: u16,
    },
    Reverse {
        device: Option<String>,
        device_port: u16,
        host_port: u16,
    },
}

struct PtyControlOptions {
    device: Option<String>,
    program: String,
    args: Vec<String>,
    run_as: RunAs,
    cols: u16,
    rows: u16,
    term: String,
    echo: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum ControlResponse {
    Devices {
        devices: Vec<DeviceInfo>,
    },
    Pong {
        latency_ms: u64,
    },
    Exec {
        stdout: String,
        stderr: String,
        code: Option<i32>,
    },
    Clipboard {
        text: String,
    },
    Ok {
        message: String,
    },
    Error {
        message: String,
    },
}
pub(super) async fn run_server(
    bind: &str,
    control: &str,
    connect: Vec<String>,
    discover: bool,
    insecure: bool,
) -> Result<()> {
    let security = if insecure {
        SecurityMode::Insecure
    } else {
        SecurityMode::Secure
    };
    if insecure {
        tracing::warn!(
            "INSECURE MODE: SideWire authentication and traffic encryption are disabled"
        );
    }
    let devices: DeviceMap = Arc::new(RwLock::new(HashMap::new()));
    tracing::info!(%bind, %control, inbound_targets = connect.len(), discover, security = security.as_str(), "SideWire server starting");

    for endpoint in connect {
        let devices = devices.clone();
        tokio::spawn(async move {
            device_connector(endpoint, devices, security).await;
        });
    }
    if discover {
        let devices = devices.clone();
        tokio::spawn(async move {
            discovered_device_manager(devices, security).await;
        });
    }

    let device_task = device_listener(bind, devices.clone(), security);
    let control_task = control_listener(control, devices);
    tokio::try_join!(device_task, control_task)?;
    Ok(())
}

async fn device_listener(bind: &str, devices: DeviceMap, security: SecurityMode) -> Result<()> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind device listener {bind}"))?;
    tracing::info!(%bind, "waiting for outbound SideWire devices");
    loop {
        let (mut stream, peer) = listener.accept().await?;
        stream.set_nodelay(true).context("enable TCP_NODELAY")?;
        let peer_text = peer.to_string();
        let device_ip = peer.ip();
        let local_ip = stream.local_addr()?.ip();
        match accept_device(&mut stream, security).await {
            Ok(identity) => {
                let transport = DeviceTransport::new(
                    stream,
                    identity.noise.clone(),
                    identity.protocol,
                    identity.capabilities,
                );
                let session = DeviceSession {
                    id: identity.id,
                    name: identity.name,
                    peer: peer_text,
                    mode: ConnectionMode::Outbound,
                    device_ip,
                    local_ip,
                    transport: transport.clone(),
                    security: identity.security,
                    shared_secret: identity.shared_secret,
                    capabilities: identity.capabilities,
                    protocol: identity.protocol,
                };
                if let Err(error) = register_device(&devices, session).await {
                    tracing::warn!(%peer, %error, "outbound device rejected");
                }
            }
            Err(error) => tracing::warn!(%peer, %error, "device handshake failed"),
        }
    }
}

struct DeviceIdentity {
    id: DeviceId,
    name: String,
    security: SecurityMode,
    noise: Option<SharedNoise>,
    shared_secret: Option<[u8; 32]>,
    capabilities: u64,
    protocol: u16,
}

async fn accept_security(
    stream: &mut TcpStream,
    security: SecurityMode,
) -> Result<(DeviceId, Option<SharedNoise>, Option<[u8; 32]>)> {
    let host_id = crate::trust::host_id()?;
    write_packet(
        stream,
        &SecurityBanner {
            node_id: host_id,
            security,
            protocol_version: sidewire_protocol::VERSION,
        },
    )
    .await?;
    let hello: SecurityClientHello = read_packet(stream).await?;
    if !protocol_compatible(hello.protocol_version) {
        let message = format!(
            "protocol mismatch: peer {}, host {}",
            protocol_label(hello.protocol_version),
            protocol_label(sidewire_protocol::VERSION)
        );
        write_packet(
            stream,
            &SecurityDecision {
                accepted: false,
                message: message.clone(),
            },
        )
        .await?;
        bail!(message);
    }
    if hello.security != security {
        let message = format!(
            "security mode mismatch: peer {}, host {}",
            hello.security.as_str(),
            security.as_str()
        );
        write_packet(
            stream,
            &SecurityDecision {
                accepted: false,
                message: message.clone(),
            },
        )
        .await?;
        bail!(message);
    }
    let secret = match security {
        SecurityMode::Secure => match crate::trust::device_secret(hello.node_id)? {
            Some(secret) => Some(secret),
            None => {
                let message = format!(
                    "device {} is not paired; run sidewire pair first",
                    hello.node_id.short()
                );
                write_packet(
                    stream,
                    &SecurityDecision {
                        accepted: false,
                        message: message.clone(),
                    },
                )
                .await?;
                bail!(message);
            }
        },
        SecurityMode::Insecure => None,
    };
    write_packet(
        stream,
        &SecurityDecision {
            accepted: true,
            message: "ok".into(),
        },
    )
    .await?;
    let noise = if let Some(secret) = secret {
        let version =
            negotiated_version(hello.protocol_version).context("no compatible protocol version")?;
        let prologue = security_prologue(version, hello.node_id, host_id);
        Some(noise_responder(stream, &secret, &prologue).await?)
    } else {
        None
    };
    Ok((hello.node_id, noise, secret))
}

async fn connect_security(
    stream: &mut TcpStream,
    security: SecurityMode,
) -> Result<(DeviceId, Option<SharedNoise>, Option<[u8; 32]>)> {
    let banner: SecurityBanner = read_packet(stream).await?;
    if !protocol_compatible(banner.protocol_version) {
        bail!(
            "protocol mismatch: device {}, host {}",
            protocol_label(banner.protocol_version),
            protocol_label(sidewire_protocol::VERSION)
        );
    }
    if banner.security != security {
        bail!(
            "security mode mismatch: device {}, host {}; both sides must explicitly use the same mode",
            banner.security.as_str(),
            security.as_str()
        );
    }
    let host_id = crate::trust::host_id()?;
    let secret = match security {
        SecurityMode::Secure => Some(crate::trust::device_secret(banner.node_id)?.with_context(
            || {
                format!(
                    "device {} is not paired; run sidewire pair first",
                    banner.node_id.short()
                )
            },
        )?),
        SecurityMode::Insecure => None,
    };
    write_packet(
        stream,
        &SecurityClientHello {
            node_id: host_id,
            security,
            protocol_version: sidewire_protocol::VERSION,
        },
    )
    .await?;
    let decision: SecurityDecision = read_packet(stream).await?;
    if !decision.accepted {
        bail!(decision.message);
    }
    let noise = if let Some(secret) = secret {
        let version = negotiated_version(banner.protocol_version)
            .context("no compatible protocol version")?;
        let prologue = security_prologue(version, host_id, banner.node_id);
        Some(noise_initiator(stream, &secret, &prologue).await?)
    } else {
        None
    };
    Ok((banner.node_id, noise, secret))
}

async fn accept_device(stream: &mut TcpStream, security: SecurityMode) -> Result<DeviceIdentity> {
    let (security_id, noise, shared_secret) = accept_security(stream, security).await?;
    let hello_frame = {
        let mut reader = SecureFrameReader::new(&mut *stream, noise.clone());
        reader.read_frame().await?
    };
    if hello_frame.kind != FrameKind::Hello {
        bail!("expected device Hello");
    }
    let hello: Hello = decode(&hello_frame.payload)?;
    let protocol = negotiated_version(hello.protocol_version)
        .context("device protocol major is incompatible")?;
    let capabilities = hello.capabilities & sidewire_protocol::capabilities::ALL;
    if capabilities & sidewire_protocol::capabilities::CORE == 0 {
        bail!("device does not advertise the core capability");
    }
    let id = hello
        .device_id
        .context("device Hello did not include device_id")?;
    if id != security_id {
        bail!("device identity changed after security handshake");
    }
    let ack = HelloAck {
        device_id: None,
        name: "sidewire-server".into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        protocol_version: sidewire_protocol::VERSION,
        capabilities: sidewire_protocol::capabilities::ALL,
    };
    {
        let mut writer = SecureFrameWriter::new(&mut *stream, noise.clone());
        writer
            .write_frame(&frame(FrameKind::HelloAck, 0, &ack)?)
            .await?;
    }
    Ok(DeviceIdentity {
        id,
        name: hello.name,
        security,
        noise,
        shared_secret,
        capabilities,
        protocol,
    })
}

async fn connect_device(stream: &mut TcpStream, security: SecurityMode) -> Result<DeviceIdentity> {
    let (security_id, noise, shared_secret) = connect_security(stream, security).await?;
    let hello = Hello {
        device_id: None,
        name: "sidewire-server".into(),
        role: sidewire_protocol::PeerRole::Host,
        protocol_version: sidewire_protocol::VERSION,
        capabilities: sidewire_protocol::capabilities::ALL,
    };
    {
        let mut writer = SecureFrameWriter::new(&mut *stream, noise.clone());
        writer
            .write_frame(&frame(FrameKind::Hello, 0, &hello)?)
            .await?;
    }
    let ack_frame = {
        let mut reader = SecureFrameReader::new(&mut *stream, noise.clone());
        reader.read_frame().await?
    };
    if ack_frame.kind != FrameKind::HelloAck {
        bail!("expected device HelloAck");
    }
    let ack: HelloAck = decode(&ack_frame.payload)?;
    let protocol = negotiated_version(ack.protocol_version)
        .context("device protocol major is incompatible")?;
    let capabilities = ack.capabilities & sidewire_protocol::capabilities::ALL;
    if capabilities & sidewire_protocol::capabilities::CORE == 0 {
        bail!("device does not advertise the core capability");
    }
    let id = ack
        .device_id
        .context("device HelloAck did not include device_id")?;
    if id != security_id {
        bail!("device identity changed after security handshake");
    }
    if !protocol_compatible(ack.protocol_version) {
        bail!(
            "protocol mismatch after HelloAck: device {}",
            protocol_label(ack.protocol_version)
        );
    }
    if ack.name.trim().is_empty() {
        bail!("device returned an empty name");
    }
    Ok(DeviceIdentity {
        id,
        name: ack.name,
        security,
        noise,
        shared_secret,
        capabilities,
        protocol,
    })
}

async fn register_device(devices: &DeviceMap, session: DeviceSession) -> Result<()> {
    let id = session.id;
    let name = session.name.clone();
    let mode = session.mode;
    let peer = session.peer.clone();
    let monitor = session.clone();
    let previous = devices.write().await.insert(id, session);
    if let Some(existing) = previous
        && !existing.transport.same_connection(&monitor.transport)
        && !existing.transport.is_closed()
    {
        tracing::warn!(
            device = %name,
            device_id = %id.short(),
            old_peer = %existing.peer,
            new_peer = %peer,
            "replacing stale device session"
        );
        tokio::spawn(async move {
            existing.transport.close().await;
        });
    }
    tracing::info!(device = %name, device_id = %id.short(), mode = mode.as_str(), %peer, "device connected");
    if monitor.capabilities & sidewire_protocol::capabilities::HEARTBEAT != 0 {
        tokio::spawn(device_heartbeat(monitor.clone()));
    }
    let devices = devices.clone();
    tokio::spawn(async move {
        monitor.transport.wait_closed().await;
        remove_device_if_same(&devices, id, &monitor).await;
    });
    Ok(())
}

async fn device_heartbeat(session: DeviceSession) {
    let mut misses = 0u32;
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS)).await;
        if session.transport.is_closed() {
            return;
        }
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(HEARTBEAT_TIMEOUT_SECS),
            remote_ping(&session),
        )
        .await;
        match result {
            Ok(Ok(rtt_ms)) => {
                if misses > 0 {
                    tracing::info!(
                        device = %session.name,
                        device_id = %session.id.short(),
                        rtt_ms,
                        previous_misses = misses,
                        "device heartbeat recovered"
                    );
                }
                misses = 0;
            }
            Ok(Err(error)) => {
                if session.transport.is_closed() {
                    return;
                }
                misses += 1;
                tracing::warn!(
                    device = %session.name,
                    device_id = %session.id.short(),
                    misses,
                    max_misses = HEARTBEAT_MAX_MISSES,
                    %error,
                    "device heartbeat failed"
                );
            }
            Err(_) => {
                misses += 1;
                tracing::warn!(
                    device = %session.name,
                    device_id = %session.id.short(),
                    misses,
                    max_misses = HEARTBEAT_MAX_MISSES,
                    "device heartbeat timed out"
                );
            }
        }
        if misses >= HEARTBEAT_MAX_MISSES {
            tracing::warn!(
                device = %session.name,
                device_id = %session.id.short(),
                "closing device after consecutive heartbeat failures"
            );
            session.transport.close().await;
            return;
        }
    }
}

async fn connect_endpoint_once(
    endpoint: &str,
    expected_id: Option<DeviceId>,
    devices: &DeviceMap,
    security: SecurityMode,
) -> Result<()> {
    tracing::info!(%endpoint, "connecting to inbound SideWire device");
    let mut stream = TcpStream::connect(endpoint)
        .await
        .with_context(|| format!("connect inbound device {endpoint}"))?;
    stream.set_nodelay(true).context("enable TCP_NODELAY")?;
    let peer = stream.peer_addr()?;
    let local = stream.local_addr()?;
    let identity = connect_device(&mut stream, security).await?;
    if let Some(expected) = expected_id
        && identity.id != expected
    {
        bail!(
            "discovery identity changed at {endpoint}: expected {}, got {}",
            expected.short(),
            identity.id.short()
        );
    }
    let transport = DeviceTransport::new(
        stream,
        identity.noise.clone(),
        identity.protocol,
        identity.capabilities,
    );
    let session = DeviceSession {
        id: identity.id,
        name: identity.name,
        peer: peer.to_string(),
        mode: ConnectionMode::Inbound,
        device_ip: peer.ip(),
        local_ip: local.ip(),
        transport: transport.clone(),
        security: identity.security,
        shared_secret: identity.shared_secret,
        capabilities: identity.capabilities,
        protocol: identity.protocol,
    };
    register_device(devices, session).await?;
    transport.wait_closed().await;
    Ok(())
}

async fn device_connector(endpoint: String, devices: DeviceMap, security: SecurityMode) {
    let mut delay = 1u64;
    loop {
        match connect_endpoint_once(&endpoint, None, &devices, security).await {
            Ok(()) => delay = 1,
            Err(error) => tracing::warn!(%endpoint, %error, "inbound device connect failed"),
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}

async fn discovered_device_connector(
    device_id: DeviceId,
    endpoints: Arc<RwLock<HashMap<DeviceId, String>>>,
    devices: DeviceMap,
    security: SecurityMode,
) {
    let mut delay = 1u64;
    loop {
        let connected = devices
            .read()
            .await
            .get(&device_id)
            .is_some_and(|session| !session.transport.is_closed());
        if connected {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            delay = 1;
            continue;
        }
        let endpoint = endpoints.read().await.get(&device_id).cloned();
        let Some(endpoint) = endpoint else {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            continue;
        };
        match connect_endpoint_once(&endpoint, Some(device_id), &devices, security).await {
            Ok(()) => delay = 1,
            Err(error) => {
                tracing::debug!(device_id = %device_id.short(), %endpoint, %error, "discovered device connect failed")
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(delay)).await;
        delay = (delay * 2).min(10);
    }
}

async fn discovered_device_manager(devices: DeviceMap, security: SecurityMode) {
    let endpoints = Arc::new(RwLock::new(HashMap::<DeviceId, String>::new()));
    let mut started = HashSet::new();
    loop {
        match crate::discovery::discover_all(tokio::time::Duration::from_millis(1200)).await {
            Ok(found) => {
                for device in found {
                    if !sidewire_protocol::protocol_compatible(device.protocol_version) {
                        tracing::warn!(
                            device = %device.name,
                            device_id = %device.device_id.short(),
                            device_protocol = %sidewire_protocol::protocol_label(device.protocol_version),
                            host_protocol = %sidewire_protocol::protocol_label(sidewire_protocol::VERSION),
                            "ignoring discovered device with incompatible protocol"
                        );
                        continue;
                    }
                    if device.security != security {
                        tracing::debug!(device = %device.name, device_security = device.security.as_str(), host_security = security.as_str(), "ignoring discovered device with different security mode");
                        continue;
                    }
                    endpoints
                        .write()
                        .await
                        .insert(device.device_id, device.endpoint.clone());
                    if started.insert(device.device_id) {
                        let endpoints = endpoints.clone();
                        let devices = devices.clone();
                        tokio::spawn(discovered_device_connector(
                            device.device_id,
                            endpoints,
                            devices,
                            security,
                        ));
                    }
                }
            }
            Err(error) => tracing::debug!(%error, "SideWire discovery failed"),
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    }
}

#[cfg(windows)]
async fn control_listener(control: &str, devices: DeviceMap) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let mut first = true;
    loop {
        let server = ServerOptions::new()
            .reject_remote_clients(true)
            .first_pipe_instance(first)
            .create(control)
            .map_err(|error| {
                if first && error.raw_os_error() == Some(5) { anyhow::anyhow!("SideWire control pipe {control} is already in use; another SideWire server may already be running") }
                else { anyhow::anyhow!("create control pipe {control}: {error}") }
            })?;
        first = false;
        if let Err(error) = server.connect().await {
            tracing::warn!(%control, %error, "local control pipe connect failed; retrying");
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            continue;
        }
        tracing::info!(%control, "local CLI control connected");
        let devices = devices.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_control(server, devices).await {
                tracing::warn!(%error, "control request failed");
            }
        });
    }
}

#[cfg(unix)]
async fn control_listener(control: &str, devices: DeviceMap) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::{UnixListener, UnixStream};

    let path = std::path::Path::new(control);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            bail!("SideWire control socket is already active at {control}");
        }
        let _ = std::fs::remove_file(path);
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("bind control socket {control}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(%control, "local CLI control ready");
    loop {
        let (stream, _) = listener.accept().await?;
        let devices = devices.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_control(stream, devices).await {
                tracing::warn!(%error, "control request failed");
            }
        });
    }
}

async fn handle_control<S>(stream: S, devices: DeviceMap) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        bail!("empty control request");
    }
    let request: ControlRequest = serde_json::from_str(line.trim_end())?;
    match request {
        ControlRequest::ExecStream {
            device,
            program,
            args,
            cwd,
            run_as,
        } => handle_control_exec_stream(reader, &devices, device, program, args, cwd, run_as).await,
        ControlRequest::Pty {
            device,
            program,
            args,
            run_as,
            cols,
            rows,
            term,
            echo,
        } => {
            let options = PtyControlOptions {
                device,
                program,
                args,
                run_as,
                cols,
                rows,
                term,
                echo,
            };
            handle_control_pty(reader, &devices, options).await
        }
        other => {
            let response = process_control(other, &devices).await;
            let mut encoded = serde_json::to_vec(&response)?;
            encoded.push(b'\n');
            reader.get_mut().write_all(&encoded).await?;
            reader.get_mut().flush().await?;
            Ok(())
        }
    }
}

async fn handle_control_exec_stream<S>(
    mut reader: BufReader<S>,
    devices: &DeviceMap,
    device: Option<String>,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    run_as: RunAs,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (_, session) = match resolve_device(devices, device.as_deref()).await {
        Ok(selected) => selected,
        Err(error) => {
            let mut encoded = serde_json::to_vec(&ControlResponse::Error {
                message: error.to_string(),
            })?;
            encoded.push(b'\n');
            reader.get_mut().write_all(&encoded).await?;
            return Ok(());
        }
    };
    let mut remote = session.transport.open_stream().await?;
    let stream_id = remote.id();
    let request = ExecRequest {
        program,
        args,
        cwd,
        identity: run_as.into(),
    };
    remote
        .send(&frame(FrameKind::ExecRequest, stream_id, &request)?)
        .await?;

    let mut encoded = serde_json::to_vec(&ControlResponse::Ok {
        message: "exec stream ready".into(),
    })?;
    encoded.push(b'\n');
    reader.get_mut().write_all(&encoded).await?;

    loop {
        let frame = remote.recv().await?;
        let done = matches!(frame.kind, FrameKind::ExecExit | FrameKind::Error);
        if write_frame(reader.get_mut(), &frame).await.is_err() {
            return Ok(());
        }
        if done {
            return Ok(());
        }
    }
}

async fn handle_control_pty<S>(
    mut reader: BufReader<S>,
    devices: &DeviceMap,
    options: PtyControlOptions,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let PtyControlOptions {
        device,
        program,
        args,
        run_as,
        cols,
        rows,
        term,
        echo,
    } = options;
    let (_, session) = match resolve_device(devices, device.as_deref()).await {
        Ok(selected) => selected,
        Err(error) => {
            let mut encoded = serde_json::to_vec(&ControlResponse::Error {
                message: error.to_string(),
            })?;
            encoded.push(b'\n');
            reader.get_mut().write_all(&encoded).await?;
            return Ok(());
        }
    };
    let mut device_stream = session.transport.open_stream().await?;
    let stream_id = device_stream.id();
    let request = PtyOpenRequest {
        program,
        args,
        identity: run_as.into(),
        cols,
        rows,
        term,
        echo,
    };
    device_stream
        .send(&frame(FrameKind::PtyOpen, stream_id, &request)?)
        .await?;
    let opened = device_stream.recv().await?;
    match opened.kind {
        FrameKind::PtyOpenAck => {
            let ack: PtyOpenAck = decode(&opened.payload)?;
            let mut encoded = serde_json::to_vec(&ControlResponse::Ok {
                message: format!("pty pid={}", ack.pid),
            })?;
            encoded.push(b'\n');
            reader.get_mut().write_all(&encoded).await?;
            reader.get_mut().flush().await?;
        }
        FrameKind::Error => {
            let mut encoded = serde_json::to_vec(&ControlResponse::Error {
                message: String::from_utf8_lossy(&opened.payload).into_owned(),
            })?;
            encoded.push(b'\n');
            reader.get_mut().write_all(&encoded).await?;
            reader.get_mut().flush().await?;
            return Ok(());
        }
        kind => bail!("unexpected PTY open response {kind:?}"),
    }

    let transport = session.transport.clone();
    let (mut local_read, mut local_write) = tokio::io::split(reader);
    let local_to_device = async {
        loop {
            match read_frame(&mut local_read).await {
                Ok(mut local) => match local.kind {
                    FrameKind::PtyInput
                    | FrameKind::PtyResize
                    | FrameKind::PtyClose
                    | FrameKind::PtyComplete => {
                        local.stream_id = stream_id;
                        transport.send_frame(&local).await?;
                        if local.kind == FrameKind::PtyClose {
                            return Ok::<(), anyhow::Error>(());
                        }
                    }
                    kind => bail!("unexpected local PTY frame {kind:?}"),
                },
                Err(_) => {
                    let _ = transport
                        .send_raw(FrameKind::PtyClose, stream_id, &[])
                        .await;
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
    };
    let device_to_local = async {
        let mut local_alive = true;
        loop {
            let remote = device_stream.recv().await?;
            if remote.stream_id != stream_id {
                bail!("unexpected remote stream {} during PTY", remote.stream_id);
            }
            let done = matches!(remote.kind, FrameKind::PtyExit | FrameKind::Error);
            if local_alive && write_frame(&mut local_write, &remote).await.is_err() {
                local_alive = false;
            }
            if done {
                return Ok::<(), anyhow::Error>(());
            }
        }
    };
    tokio::pin!(local_to_device);
    tokio::pin!(device_to_local);
    tokio::select! {
        remote = &mut device_to_local => remote?,
        local = &mut local_to_device => {
            local?;
            match tokio::time::timeout(
                tokio::time::Duration::from_secs(3),
                &mut device_to_local,
            ).await {
                Ok(remote) => remote?,
                Err(_) => bail!("PTY did not terminate after client disconnect"),
            }
        }
    }
    Ok(())
}

async fn process_control(request: ControlRequest, devices: &DeviceMap) -> ControlResponse {
    match request {
        ControlRequest::Devices => {
            let guard = devices.read().await;
            let mut list: Vec<DeviceInfo> = guard
                .iter()
                .map(|(id, session)| DeviceInfo {
                    id: id.to_hex(),
                    name: session.name.clone(),
                    peer: session.peer.clone(),
                    mode: session.mode.as_str().to_owned(),
                    security: session.security.as_str().to_owned(),
                    protocol: protocol_label(session.protocol),
                    capabilities: session.capabilities,
                })
                .collect();
            list.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
            ControlResponse::Devices { devices: list }
        }
        ControlRequest::Ping { device } => match resolve_device(devices, device.as_deref()).await {
            Ok((_, session)) => match remote_ping(&session).await {
                Ok(latency_ms) => ControlResponse::Pong { latency_ms },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            },
            Err(error) => ControlResponse::Error {
                message: error.to_string(),
            },
        },
        ControlRequest::Exec {
            device,
            program,
            args,
            cwd,
            run_as,
        } => match resolve_device(devices, device.as_deref()).await {
            Ok((_, session)) => match remote_exec(&session, program, args, cwd, run_as).await {
                Ok((stdout, stderr, code)) => ControlResponse::Exec {
                    stdout,
                    stderr,
                    code,
                },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            },
            Err(error) => ControlResponse::Error {
                message: error.to_string(),
            },
        },
        ControlRequest::Push {
            device,
            local,
            remote,
            run_as,
        } => match resolve_device(devices, device.as_deref()).await {
            Ok((_, session)) => match remote_push(&session, &local, remote, run_as).await {
                Ok(bytes) => ControlResponse::Ok {
                    message: format!("pushed {bytes} bytes"),
                },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            },
            Err(error) => ControlResponse::Error {
                message: error.to_string(),
            },
        },
        ControlRequest::Pull {
            device,
            remote,
            local,
            run_as,
        } => match resolve_device(devices, device.as_deref()).await {
            Ok((_, session)) => match remote_pull(&session, remote, &local, run_as).await {
                Ok(bytes) => ControlResponse::Ok {
                    message: format!("pulled {bytes} bytes"),
                },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            },
            Err(error) => ControlResponse::Error {
                message: error.to_string(),
            },
        },
        ControlRequest::ExecStream { .. } => ControlResponse::Error {
            message: "exec stream request must use streaming control".into(),
        },
        ControlRequest::Pty { .. } => ControlResponse::Error {
            message: "PTY request must use streaming control".into(),
        },
        ControlRequest::ClipboardGet { device } => {
            match resolve_device(devices, device.as_deref()).await {
                Ok((_, session)) => match remote_clipboard_get(&session).await {
                    Ok(text) => ControlResponse::Clipboard { text },
                    Err(error) => ControlResponse::Error {
                        message: error.to_string(),
                    },
                },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            }
        }
        ControlRequest::ClipboardSet { device, text } => {
            match resolve_device(devices, device.as_deref()).await {
                Ok((_, session)) => match remote_clipboard_set(&session, text).await {
                    Ok(()) => ControlResponse::Ok {
                        message: "Android clipboard updated".into(),
                    },
                    Err(error) => ControlResponse::Error {
                        message: error.to_string(),
                    },
                },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            }
        }
        ControlRequest::ClipboardClear { device } => {
            match resolve_device(devices, device.as_deref()).await {
                Ok((_, session)) => match remote_clipboard_clear(&session).await {
                    Ok(()) => ControlResponse::Ok {
                        message: "Android clipboard cleared".into(),
                    },
                    Err(error) => ControlResponse::Error {
                        message: error.to_string(),
                    },
                },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            }
        }
        ControlRequest::Forward {
            device,
            local_port,
            remote_port,
        } => match resolve_device(devices, device.as_deref()).await {
            Ok((_, session)) => match start_forward(&session, local_port, remote_port).await {
                Ok(message) => ControlResponse::Ok { message },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            },
            Err(error) => ControlResponse::Error {
                message: error.to_string(),
            },
        },
        ControlRequest::Reverse {
            device,
            device_port,
            host_port,
        } => match resolve_device(devices, device.as_deref()).await {
            Ok((_, session)) => match start_reverse(&session, device_port, host_port).await {
                Ok(message) => ControlResponse::Ok { message },
                Err(error) => ControlResponse::Error {
                    message: error.to_string(),
                },
            },
            Err(error) => ControlResponse::Error {
                message: error.to_string(),
            },
        },
    }
}

fn selector_is_id_prefix(selector: &str) -> bool {
    let compact = selector.replace('-', "").to_ascii_lowercase();
    compact.len() >= 4 && compact.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn selector_matches_id(id: DeviceId, selector: &str) -> bool {
    if !selector_is_id_prefix(selector) {
        return false;
    }
    id.to_hex()
        .starts_with(&selector.replace('-', "").to_ascii_lowercase())
}

async fn resolve_device(
    devices: &DeviceMap,
    requested: Option<&str>,
) -> Result<(DeviceId, DeviceSession)> {
    let guard = devices.read().await;
    if let Some(selector) = requested {
        let mut matches: Vec<_> = guard
            .iter()
            .filter(|(id, _)| selector_matches_id(**id, selector))
            .map(|(id, session)| (*id, session.clone()))
            .collect();
        if matches.is_empty() {
            matches = guard
                .iter()
                .filter(|(_, session)| session.name.eq_ignore_ascii_case(selector))
                .map(|(id, session)| (*id, session.clone()))
                .collect();
        }
        match matches.len() {
            1 => return Ok(matches.pop().unwrap()),
            0 => bail!("device selector '{selector}' did not match any connected device"),
            _ => {
                let choices = matches
                    .iter()
                    .map(|(id, session)| format!("{}:{}", id.short(), session.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("device selector '{selector}' is ambiguous: {choices}; use an ID prefix");
            }
        }
    }

    match guard.len() {
        0 => bail!("no SideWire devices connected"),
        1 => {
            let (id, session) = guard.iter().next().unwrap();
            Ok((*id, session.clone()))
        }
        _ => bail!(
            "multiple devices connected; select one with -s <name-or-id> or configure default-device"
        ),
    }
}

async fn remove_device_if_same(devices: &DeviceMap, id: DeviceId, session: &DeviceSession) {
    let should_remove = devices
        .read()
        .await
        .get(&id)
        .map(|current| current.transport.same_connection(&session.transport))
        .unwrap_or(false);
    if should_remove {
        devices.write().await.remove(&id);
        tracing::info!(device = %session.name, device_id = %id.short(), "device disconnected");
    }
}

async fn remote_ping(session: &DeviceSession) -> Result<u64> {
    let mut stream = session.transport.open_stream().await?;
    let nonce = rand::random::<[u8; 8]>();
    let started = std::time::Instant::now();
    stream.send_raw(FrameKind::Ping, &nonce).await?;
    let response = stream.recv().await?;
    match response.kind {
        FrameKind::Pong if response.payload.as_slice() == nonce.as_slice() => {
            Ok(started.elapsed().as_millis().min(u64::MAX as u128) as u64)
        }
        FrameKind::Pong => bail!("ping nonce mismatch"),
        FrameKind::Error => bail!(
            "remote ping error: {}",
            String::from_utf8_lossy(&response.payload)
        ),
        other => bail!("unexpected ping response {other:?}"),
    }
}

fn require_capability(session: &DeviceSession, capability: u64, name: &str) -> Result<()> {
    if session.capabilities & capability == 0 {
        bail!("device does not support {name}");
    }
    Ok(())
}

async fn remote_clipboard_get(session: &DeviceSession) -> Result<String> {
    require_capability(
        session,
        sidewire_protocol::capabilities::CLIPBOARD,
        "clipboard",
    )?;
    let mut stream = session.transport.open_stream().await?;
    stream.send_raw(FrameKind::ClipboardGet, &[]).await?;
    let response = stream.recv().await?;
    match response.kind {
        FrameKind::ClipboardData => Ok(decode::<ClipboardData>(&response.payload)?
            .text
            .unwrap_or_default()),
        FrameKind::Error => bail!(
            "remote clipboard error: {}",
            String::from_utf8_lossy(&response.payload)
        ),
        other => bail!("unexpected clipboard response {other:?}"),
    }
}

async fn remote_clipboard_set(session: &DeviceSession, text: String) -> Result<()> {
    require_capability(
        session,
        sidewire_protocol::capabilities::CLIPBOARD,
        "clipboard",
    )?;
    let mut stream = session.transport.open_stream().await?;
    let request = ClipboardSetRequest { text };
    stream
        .send(&frame(FrameKind::ClipboardSet, stream.id(), &request)?)
        .await?;
    let response = stream.recv().await?;
    match response.kind {
        FrameKind::ClipboardData => Ok(()),
        FrameKind::Error => bail!(
            "remote clipboard error: {}",
            String::from_utf8_lossy(&response.payload)
        ),
        other => bail!("unexpected clipboard response {other:?}"),
    }
}

async fn remote_clipboard_clear(session: &DeviceSession) -> Result<()> {
    require_capability(
        session,
        sidewire_protocol::capabilities::CLIPBOARD,
        "clipboard",
    )?;
    let mut stream = session.transport.open_stream().await?;
    stream.send_raw(FrameKind::ClipboardClear, &[]).await?;
    let response = stream.recv().await?;
    match response.kind {
        FrameKind::ClipboardData => Ok(()),
        FrameKind::Error => bail!(
            "remote clipboard error: {}",
            String::from_utf8_lossy(&response.payload)
        ),
        other => bail!("unexpected clipboard response {other:?}"),
    }
}

async fn remote_exec(
    session: &DeviceSession,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    run_as: RunAs,
) -> Result<(String, String, Option<i32>)> {
    let mut stream = session.transport.open_stream().await?;
    let stream_id = stream.id();
    let request = ExecRequest {
        program,
        args,
        cwd,
        identity: run_as.into(),
    };
    stream
        .send(&frame(FrameKind::ExecRequest, stream_id, &request)?)
        .await?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let response = stream.recv().await?;
        match response.kind {
            FrameKind::ExecStdout => stdout.extend_from_slice(&response.payload),
            FrameKind::ExecStderr => stderr.extend_from_slice(&response.payload),
            FrameKind::ExecExit => {
                let exit: ExecExit = decode(&response.payload)?;
                return Ok((
                    String::from_utf8_lossy(&stdout).into_owned(),
                    String::from_utf8_lossy(&stderr).into_owned(),
                    exit.code,
                ));
            }
            FrameKind::Error => bail!(
                "remote error: {}",
                String::from_utf8_lossy(&response.payload)
            ),
            _ => {}
        }
    }
}
async fn remote_push(
    session: &DeviceSession,
    local: &str,
    remote: String,
    run_as: RunAs,
) -> Result<u64> {
    let mut file = File::open(local)
        .await
        .with_context(|| format!("open local file {local}"))?;
    let size = file.metadata().await?.len();
    let mut stream = session.transport.open_stream().await?;
    let stream_id = stream.id();
    let request = FilePushRequest {
        path: remote,
        identity: run_as.into(),
    };
    stream
        .send(&frame(FrameKind::PushRequest, stream_id, &request)?)
        .await?;
    let ready = stream.recv().await?;
    match ready.kind {
        FrameKind::FileMeta => {
            let _: FileMeta = decode(&ready.payload)?;
        }
        FrameKind::Error => bail!(
            "remote push error: {}",
            String::from_utf8_lossy(&ready.payload)
        ),
        other => bail!("unexpected push response {other:?}"),
    }
    let mut buffer = vec![0u8; FILE_BUFFER_SIZE];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        stream.send_raw(FrameKind::FileChunk, &buffer[..n]).await?;
        if let Some(response) = stream.try_recv()? {
            match response.kind {
                FrameKind::Error => bail!(
                    "remote push error: {}",
                    String::from_utf8_lossy(&response.payload)
                ),
                FrameKind::FileEnd => bail!("remote ended push before upload completed"),
                other => bail!("unexpected push response during upload {other:?}"),
            }
        }
    }
    stream.send_raw(FrameKind::FileEnd, &[]).await?;
    let done = stream.recv().await?;
    match done.kind {
        FrameKind::FileEnd => Ok(size),
        FrameKind::Error => bail!(
            "remote push error: {}",
            String::from_utf8_lossy(&done.payload)
        ),
        other => bail!("unexpected push completion {other:?}"),
    }
}

async fn remote_pull(
    session: &DeviceSession,
    remote: String,
    local: &str,
    run_as: RunAs,
) -> Result<u64> {
    let mut stream = session.transport.open_stream().await?;
    let stream_id = stream.id();
    let request = FilePullRequest {
        path: remote,
        identity: run_as.into(),
    };
    stream
        .send(&frame(FrameKind::PullRequest, stream_id, &request)?)
        .await?;
    let meta_frame = stream.recv().await?;
    let meta = match meta_frame.kind {
        FrameKind::FileMeta => decode::<FileMeta>(&meta_frame.payload)?,
        FrameKind::Error => bail!(
            "remote pull error: {}",
            String::from_utf8_lossy(&meta_frame.payload)
        ),
        other => bail!("unexpected pull response {other:?}"),
    };
    let path = PathBuf::from(local);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = File::create(&path)
        .await
        .with_context(|| format!("create {}", path.display()))?;
    let mut written = 0u64;
    loop {
        let incoming = stream.recv().await?;
        match incoming.kind {
            FrameKind::FileChunk => {
                file.write_all(&incoming.payload).await?;
                written += incoming.payload.len() as u64;
            }
            FrameKind::FileEnd => break,
            FrameKind::Error => bail!(
                "remote pull error: {}",
                String::from_utf8_lossy(&incoming.payload)
            ),
            other => bail!("unexpected pull frame {other:?}"),
        }
    }
    file.flush().await?;
    if written != meta.size {
        bail!(
            "pull size mismatch: expected {}, got {}",
            meta.size,
            written
        );
    }
    Ok(written)
}

async fn remote_start_proxy(
    session: &DeviceSession,
    request: ProxyStartRequest,
) -> Result<ProxyStartAck> {
    let mut stream = session.transport.open_stream().await?;
    let stream_id = stream.id();
    stream
        .send(&frame(FrameKind::ProxyStartRequest, stream_id, &request)?)
        .await?;
    let response = stream.recv().await?;
    match response.kind {
        FrameKind::ProxyStartAck => Ok(decode(&response.payload)?),
        FrameKind::Error => bail!(
            "remote proxy error: {}",
            String::from_utf8_lossy(&response.payload)
        ),
        other => bail!("unexpected proxy response {other:?}"),
    }
}

async fn start_forward(
    session: &DeviceSession,
    local_port: u16,
    remote_port: u16,
) -> Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .await
        .with_context(|| format!("bind local tcp:{local_port}"))?;
    let token = rand::random::<[u8; 16]>().to_vec();
    let request = ProxyStartRequest {
        id: format!("forward-{local_port}-{remote_port}"),
        bind: "0.0.0.0:0".into(),
        target: format!("127.0.0.1:{remote_port}"),
        token: token.clone(),
        token_mode: ProxyTokenMode::Expect,
    };
    let ack = remote_start_proxy(session, request).await?;
    let proxy_port: u16 = ack
        .bound
        .rsplit(':')
        .next()
        .context("invalid proxy bind")?
        .parse()?;
    let device_ip = session.device_ip;
    let shared_secret = session.shared_secret;
    tokio::spawn(async move {
        loop {
            let Ok((mut local, peer)) = listener.accept().await else {
                break;
            };
            let _ = local.set_nodelay(true);
            let token = token.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let mut remote = TcpStream::connect((device_ip, proxy_port)).await?;
                    remote.set_nodelay(true)?;
                    if let Some(secret) = shared_secret {
                        let prologue = sidewire_protocol::proxy_prologue(&token, "forward");
                        let noise =
                            sidewire_protocol::noise_initiator(&mut remote, &secret, &prologue)
                                .await?;
                        sidewire_protocol::copy_noise_tunnel(&mut local, &mut remote, noise)
                            .await?;
                    } else {
                        remote.write_all(&token).await?;
                        tokio::io::copy_bidirectional_with_sizes(
                            &mut local,
                            &mut remote,
                            PROXY_BUFFER_SIZE,
                            PROXY_BUFFER_SIZE,
                        )
                        .await?;
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(%peer, %error, "forward connection ended");
                }
            });
        }
    });
    Ok(format!("tcp:{local_port} -> device tcp:{remote_port}"))
}

async fn start_reverse(
    session: &DeviceSession,
    device_port: u16,
    host_port: u16,
) -> Result<String> {
    let relay = TcpListener::bind(("0.0.0.0", 0)).await?;
    let relay_port = relay.local_addr()?.port();
    let token = rand::random::<[u8; 16]>().to_vec();
    let relay_token = token.clone();
    let shared_secret = session.shared_secret;
    tokio::spawn(async move {
        loop {
            let Ok((mut incoming, peer)) = relay.accept().await else {
                break;
            };
            let _ = incoming.set_nodelay(true);
            let token = relay_token.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let mut local = TcpStream::connect(("127.0.0.1", host_port)).await?;
                    local.set_nodelay(true)?;
                    if let Some(secret) = shared_secret {
                        let prologue = sidewire_protocol::proxy_prologue(&token, "reverse");
                        let noise =
                            sidewire_protocol::noise_responder(&mut incoming, &secret, &prologue)
                                .await?;
                        sidewire_protocol::copy_noise_tunnel(&mut local, &mut incoming, noise)
                            .await?;
                    } else {
                        let mut received = vec![0u8; token.len()];
                        incoming.read_exact(&mut received).await?;
                        if received != token {
                            bail!("reverse relay token mismatch");
                        }
                        tokio::io::copy_bidirectional_with_sizes(
                            &mut incoming,
                            &mut local,
                            PROXY_BUFFER_SIZE,
                            PROXY_BUFFER_SIZE,
                        )
                        .await?;
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(%peer, %error, "reverse relay ended");
                }
            });
        }
    });
    let request = ProxyStartRequest {
        id: format!("reverse-{device_port}-{host_port}"),
        bind: format!("127.0.0.1:{device_port}"),
        target: format!("{}:{relay_port}", session.local_ip),
        token,
        token_mode: ProxyTokenMode::Send,
    };
    remote_start_proxy(session, request).await?;
    Ok(format!("device tcp:{device_port} -> host tcp:{host_port}"))
}
