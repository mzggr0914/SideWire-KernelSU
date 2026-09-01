use super::*;
use crate::transport::DeviceTransport;

const FILE_BUFFER_SIZE: usize = 256 * 1024;
const PROXY_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Clone)]
struct DeviceSession {
    pub(super) peer: String,
    device_ip: IpAddr,
    local_ip: IpAddr,
    transport: DeviceTransport,
}

type DeviceMap = Arc<RwLock<HashMap<String, DeviceSession>>>;
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct DeviceInfo {
    pub(super) name: String,
    pub(super) peer: String,
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
) -> Result<()> {
    let devices: DeviceMap = Arc::new(RwLock::new(HashMap::new()));
    tracing::info!(%bind, %control, inbound_targets = connect.len(), discover, "SideWire server starting");

    for endpoint in connect {
        let devices = devices.clone();
        tokio::spawn(async move {
            device_connector(endpoint, devices).await;
        });
    }
    if discover {
        let devices = devices.clone();
        tokio::spawn(async move {
            discovered_device_connector(devices).await;
        });
    }

    let device_task = device_listener(bind, devices.clone());
    let control_task = control_listener(control, devices);
    tokio::try_join!(device_task, control_task)?;
    Ok(())
}

async fn device_listener(bind: &str, devices: DeviceMap) -> Result<()> {
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
        match accept_device(&mut stream).await {
            Ok(name) => {
                tracing::info!(device = %name, peer = %peer_text, "device connected");
                let transport = DeviceTransport::new(stream);
                let session = DeviceSession {
                    peer: peer_text,
                    device_ip,
                    local_ip,
                    transport: transport.clone(),
                };
                devices.write().await.insert(name.clone(), session.clone());
                let devices = devices.clone();
                tokio::spawn(async move {
                    transport.wait_closed().await;
                    remove_device_if_same(&devices, &name, &session).await;
                });
            }
            Err(error) => tracing::warn!(%peer, %error, "device handshake failed"),
        }
    }
}

async fn accept_device(stream: &mut TcpStream) -> Result<String> {
    let hello_frame = read_frame(stream).await?;
    if hello_frame.kind != FrameKind::Hello {
        bail!("expected device Hello");
    }
    let hello: Hello = decode(&hello_frame.payload)?;
    let ack = HelloAck {
        name: "sidewire-server".into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
    };
    write_frame(stream, &frame(FrameKind::HelloAck, 0, &ack)?).await?;
    Ok(hello.name)
}

async fn connect_device(stream: &mut TcpStream) -> Result<String> {
    let hello = Hello {
        name: "sidewire-server".into(),
        role: sidewire_protocol::PeerRole::Host,
        protocol_version: sidewire_protocol::VERSION,
    };
    write_frame(stream, &frame(FrameKind::Hello, 0, &hello)?).await?;
    let ack_frame = read_frame(stream).await?;
    if ack_frame.kind != FrameKind::HelloAck {
        bail!("expected device HelloAck");
    }
    let ack: HelloAck = decode(&ack_frame.payload)?;
    if ack.name.trim().is_empty() {
        bail!("device returned an empty name");
    }
    Ok(ack.name)
}

async fn connect_endpoint_once(endpoint: &str, devices: &DeviceMap) -> Result<()> {
    tracing::info!(%endpoint, "connecting to inbound SideWire device");
    let mut stream = TcpStream::connect(endpoint)
        .await
        .with_context(|| format!("connect inbound device {endpoint}"))?;
    stream.set_nodelay(true).context("enable TCP_NODELAY")?;
    let peer = stream.peer_addr()?;
    let local = stream.local_addr()?;
    let name = connect_device(&mut stream).await?;
    let peer_text = peer.to_string();
    tracing::info!(device = %name, peer = %peer_text, %endpoint, "inbound device connected");
    let transport = DeviceTransport::new(stream);
    let session = DeviceSession {
        peer: peer_text,
        device_ip: peer.ip(),
        local_ip: local.ip(),
        transport: transport.clone(),
    };
    devices.write().await.insert(name.clone(), session.clone());
    transport.wait_closed().await;
    remove_device_if_same(devices, &name, &session).await;
    tracing::warn!(device = %name, %endpoint, "inbound device connection ended");
    Ok(())
}

async fn device_connector(endpoint: String, devices: DeviceMap) {
    let mut delay = 1u64;
    loop {
        match connect_endpoint_once(&endpoint, &devices).await {
            Ok(()) => delay = 1,
            Err(error) => tracing::warn!(%endpoint, %error, "inbound device connect failed"),
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}

async fn discovered_device_connector(devices: DeviceMap) {
    loop {
        match crate::discovery::discover_one(tokio::time::Duration::from_millis(1500)).await {
            Ok(Some(found)) if found.protocol_version == sidewire_protocol::VERSION => {
                let endpoint = found.endpoint;
                if let Err(error) = connect_endpoint_once(&endpoint, &devices).await {
                    tracing::warn!(%endpoint, %error, "discovered device connection failed");
                }
            }
            Ok(Some(found)) => tracing::warn!(
                device = %found.name,
                device_protocol = found.protocol_version,
                host_protocol = sidewire_protocol::VERSION,
                "ignoring discovered device with incompatible protocol"
            ),
            Ok(None) => {}
            Err(error) => tracing::debug!(%error, "SideWire discovery failed"),
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}

async fn control_listener(control: &str, devices: DeviceMap) -> Result<()> {
    let listener = TcpListener::bind(control)
        .await
        .with_context(|| format!("bind control listener {control}"))?;
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

async fn handle_control(stream: TcpStream, devices: DeviceMap) -> Result<()> {
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

async fn handle_control_exec_stream(
    mut reader: BufReader<TcpStream>,
    devices: &DeviceMap,
    device: Option<String>,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    run_as: RunAs,
) -> Result<()> {
    let (_, session) = resolve_device(devices, device.as_deref()).await?;
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

async fn handle_control_pty(
    mut reader: BufReader<TcpStream>,
    devices: &DeviceMap,
    options: PtyControlOptions,
) -> Result<()> {
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
    let (_, session) = resolve_device(devices, device.as_deref()).await?;
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
                .map(|(name, session)| DeviceInfo {
                    name: name.clone(),
                    peer: session.peer.clone(),
                })
                .collect();
            list.sort_by(|a, b| a.name.cmp(&b.name));
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

async fn resolve_device(
    devices: &DeviceMap,
    requested: Option<&str>,
) -> Result<(String, DeviceSession)> {
    let guard = devices.read().await;
    if let Some(name) = requested {
        let session = guard
            .get(name)
            .cloned()
            .with_context(|| format!("device '{name}' is not connected"))?;
        return Ok((name.to_owned(), session));
    }

    match guard.len() {
        0 => bail!("no SideWire devices connected"),
        1 => {
            let (name, session) = guard.iter().next().unwrap();
            Ok((name.clone(), session.clone()))
        }
        _ => bail!("multiple devices connected; select one with -s <name>"),
    }
}

async fn remove_device_if_same(devices: &DeviceMap, name: &str, session: &DeviceSession) {
    let should_remove = devices
        .read()
        .await
        .get(name)
        .map(|current| current.transport.same_connection(&session.transport))
        .unwrap_or(false);
    if should_remove {
        devices.write().await.remove(name);
        tracing::info!(device = %name, "device disconnected");
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
                    remote.write_all(&token).await?;
                    tokio::io::copy_bidirectional_with_sizes(
                        &mut local,
                        &mut remote,
                        PROXY_BUFFER_SIZE,
                        PROXY_BUFFER_SIZE,
                    )
                    .await?;
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
    tokio::spawn(async move {
        loop {
            let Ok((mut incoming, peer)) = relay.accept().await else {
                break;
            };
            let _ = incoming.set_nodelay(true);
            let token = relay_token.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let mut received = vec![0u8; token.len()];
                    incoming.read_exact(&mut received).await?;
                    if received != token {
                        bail!("reverse relay token mismatch");
                    }
                    let mut local = TcpStream::connect(("127.0.0.1", host_port)).await?;
                    local.set_nodelay(true)?;
                    tokio::io::copy_bidirectional_with_sizes(
                        &mut incoming,
                        &mut local,
                        PROXY_BUFFER_SIZE,
                        PROXY_BUFFER_SIZE,
                    )
                    .await?;
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
