use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sidewire_protocol::{
    ExecExit, ExecIdentity, ExecRequest, FileMeta, FilePullRequest, FilePushRequest, FrameKind,
    Hello, HelloAck, ProxyStartAck, ProxyStartRequest, ProxyTokenMode, PtyCompleteRequest,
    PtyCompleteResult, PtyExit, PtyOpenAck, PtyOpenRequest, PtyResize, decode, frame, raw_frame,
    read_frame, write_frame,
};
use std::{
    collections::HashMap,
    io::{self, Write},
    net::IpAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Mutex, RwLock},
};

const DEFAULT_DEVICE_BIND: &str = "0.0.0.0:58321";
const DEFAULT_CONTROL: &str = "127.0.0.1:58322";

#[derive(Clone, Copy, Debug, ValueEnum, Serialize, Deserialize)]
enum RunAs {
    Root,
    Shell,
}
impl From<RunAs> for ExecIdentity {
    fn from(v: RunAs) -> Self {
        match v {
            RunAs::Root => Self::Root,
            RunAs::Shell => Self::Shell,
        }
    }
}

#[derive(Parser)]
#[command(name = "sidewire", version, about = "SideWire desktop CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Server {
        #[arg(long, default_value = DEFAULT_DEVICE_BIND)]
        bind: String,
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
    },
    Devices {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
    },
    Exec {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        program: String,
        args: Vec<String>,
    },
    Shell {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        /// Use per-keystroke raw console input. Default is reliable line input.
        #[arg(long)]
        raw: bool,
        /// Send a synthetic command sequence to verify the PTY path.
        #[arg(long)]
        probe: bool,
    },
    Push {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        local: PathBuf,
        remote: String,
    },
    Pull {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        remote: String,
        local: PathBuf,
    },
    Install {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        apk: PathBuf,
    },
    Uninstall {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        package: String,
    },
    Logcat {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        #[arg(long)]
        clear: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Forward {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        local: String,
        remote: String,
    },
    Reverse {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        remote: String,
        local: String,
    },
    Reboot {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "root")]
        run_as: RunAs,
        target: Option<String>,
    },
    Screencap {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        output: PathBuf,
    },
    Packages {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        filter: Option<String>,
    },
    App {
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: String,
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        #[command(subcommand)]
        command: AppCommand,
    },
}

#[derive(Subcommand)]
enum AppCommand {
    Start { package: String },
    Stop { package: String },
    Clear { package: String },
}

#[derive(Clone)]
struct DeviceSession {
    peer: String,
    device_ip: IpAddr,
    local_ip: IpAddr,
    stream: Arc<Mutex<TcpStream>>,
}

type DeviceMap = Arc<RwLock<HashMap<String, DeviceSession>>>;
#[derive(Debug, Serialize, Deserialize)]
struct DeviceInfo {
    name: String,
    peer: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ControlRequest {
    Devices,
    Exec {
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ControlResponse {
    Devices {
        devices: Vec<DeviceInfo>,
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
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    match Cli::parse().command {
        Command::Server { bind, control } => run_server(&bind, &control).await,
        Command::Devices { control } => run_devices(&control).await,
        Command::Exec {
            control,
            device,
            run_as,
            program,
            args,
        } => run_exec_client(&control, device, run_as, program, args).await,
        Command::Shell {
            control,
            device,
            run_as,
            raw,
            probe,
        } => run_shell(&control, device, run_as, raw, probe).await,
        Command::Push {
            control,
            device,
            run_as,
            local,
            remote,
        } => run_push(&control, device, run_as, local, remote).await,
        Command::Pull {
            control,
            device,
            run_as,
            remote,
            local,
        } => run_pull(&control, device, run_as, remote, local).await,
        Command::Install {
            control,
            device,
            run_as,
            apk,
        } => run_install(&control, device, run_as, apk).await,
        Command::Uninstall {
            control,
            device,
            run_as,
            package,
        } => {
            run_exec_checked(
                &control,
                device,
                run_as,
                "/system/bin/pm",
                vec!["uninstall".into(), package],
            )
            .await
        }
        Command::Logcat {
            control,
            device,
            run_as,
            clear,
            args,
        } => run_logcat(&control, device, run_as, clear, args).await,
        Command::Forward {
            control,
            device,
            local,
            remote,
        } => run_forward(&control, device, local, remote).await,
        Command::Reverse {
            control,
            device,
            remote,
            local,
        } => run_reverse(&control, device, remote, local).await,
        Command::Reboot {
            control,
            device,
            run_as,
            target,
        } => run_reboot(&control, device, run_as, target).await,
        Command::Screencap {
            control,
            device,
            run_as,
            output,
        } => run_screencap(&control, device, run_as, output).await,
        Command::Packages {
            control,
            device,
            run_as,
            filter,
        } => run_packages(&control, device, run_as, filter).await,
        Command::App {
            control,
            device,
            run_as,
            command,
        } => run_app(&control, device, run_as, command).await,
    }
}

async fn run_server(bind: &str, control: &str) -> Result<()> {
    let devices: DeviceMap = Arc::new(RwLock::new(HashMap::new()));
    tracing::info!(%bind, %control, "SideWire server starting");

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
                let session = DeviceSession {
                    peer: peer_text,
                    device_ip,
                    local_ip,
                    stream: Arc::new(Mutex::new(stream)),
                };
                devices.write().await.insert(name, session);
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
            handle_control_pty(
                reader, &devices, device, program, args, run_as, cols, rows, term, echo,
            )
            .await
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
async fn handle_control_pty(
    mut reader: BufReader<TcpStream>,
    devices: &DeviceMap,
    device: Option<String>,
    program: String,
    args: Vec<String>,
    run_as: RunAs,
    cols: u16,
    rows: u16,
    term: String,
    echo: bool,
) -> Result<()> {
    let (device_name, session) = resolve_device(devices, device.as_deref()).await?;
    let mut device_stream = session.stream.lock().await;
    let stream_id = 0x5054_5901;
    let request = PtyOpenRequest {
        program,
        args,
        identity: run_as.into(),
        cols,
        rows,
        term,
        echo,
    };
    write_frame(
        &mut *device_stream,
        &frame(FrameKind::PtyOpen, stream_id, &request)?,
    )
    .await?;
    let opened = read_frame(&mut *device_stream).await?;
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

    let forced_reset = {
        let (mut local_read, mut local_write) = tokio::io::split(reader);
        let (mut device_read, mut device_write) = tokio::io::split(&mut *device_stream);
        let local_to_device = async {
            loop {
                match read_frame(&mut local_read).await {
                    Ok(mut local) => match local.kind {
                        FrameKind::PtyInput
                        | FrameKind::PtyResize
                        | FrameKind::PtyClose
                        | FrameKind::PtyComplete => {
                            local.stream_id = stream_id;
                            write_frame(&mut device_write, &local).await?;
                            if local.kind == FrameKind::PtyClose {
                                return Ok::<(), anyhow::Error>(());
                            }
                        }
                        kind => bail!("unexpected local PTY frame {kind:?}"),
                    },
                    Err(_) => {
                        let close = raw_frame(FrameKind::PtyClose, stream_id, Vec::new());
                        let _ = write_frame(&mut device_write, &close).await;
                        return Ok::<(), anyhow::Error>(());
                    }
                }
            }
        };
        let device_to_local = async {
            let mut local_alive = true;
            loop {
                let mut remote = read_frame(&mut device_read).await?;
                if remote.stream_id != stream_id {
                    bail!("unexpected remote stream {} during PTY", remote.stream_id);
                }
                let done = matches!(remote.kind, FrameKind::PtyExit | FrameKind::Error);
                remote.stream_id = stream_id;
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
            remote = &mut device_to_local => { remote?; false }
            local = &mut local_to_device => {
                local?;
                match tokio::time::timeout(
                    tokio::time::Duration::from_secs(3),
                    &mut device_to_local,
                ).await {
                    Ok(remote) => { remote?; false }
                    Err(_) => true,
                }
            }
        }
    };

    if forced_reset {
        let _ = device_stream.shutdown().await;
        drop(device_stream);
        remove_device_if_same(devices, &device_name, &session).await;
        bail!("PTY did not terminate after client disconnect; device connection reset");
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
        ControlRequest::Exec {
            device,
            program,
            args,
            cwd,
            run_as,
        } => match resolve_device(devices, device.as_deref()).await {
            Ok((name, session)) => match remote_exec(&session, program, args, cwd, run_as).await {
                Ok((stdout, stderr, code)) => ControlResponse::Exec {
                    stdout,
                    stderr,
                    code,
                },
                Err(error) => {
                    remove_device_if_same(devices, &name, &session).await;
                    ControlResponse::Error {
                        message: error.to_string(),
                    }
                }
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
        .map(|current| Arc::ptr_eq(&current.stream, &session.stream))
        .unwrap_or(false);
    if should_remove {
        devices.write().await.remove(name);
        tracing::info!(device = %name, "device disconnected");
    }
}
async fn remote_exec(
    session: &DeviceSession,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    run_as: RunAs,
) -> Result<(String, String, Option<i32>)> {
    let mut stream = session.stream.lock().await;
    let request = ExecRequest {
        program,
        args,
        cwd,
        identity: run_as.into(),
    };
    write_frame(&mut *stream, &frame(FrameKind::ExecRequest, 1, &request)?).await?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let response = read_frame(&mut *stream).await?;
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
async fn request_control(control: &str, request: &ControlRequest) -> Result<ControlResponse> {
    let mut stream = TcpStream::connect(control)
        .await
        .with_context(|| format!("connect to SideWire server control {control}"))?;
    stream.set_nodelay(true).context("enable TCP_NODELAY")?;
    let mut encoded = serde_json::to_vec(request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        bail!("SideWire server closed control connection");
    }
    Ok(serde_json::from_str(line.trim_end())?)
}

async fn run_devices(control: &str) -> Result<()> {
    match request_control(control, &ControlRequest::Devices).await? {
        ControlResponse::Devices { devices } => {
            if devices.is_empty() {
                println!("No devices connected.");
            } else {
                println!("NAME\tPEER");
                for device in devices {
                    println!("{}\t{}", device.name, device.peer);
                }
            }
            Ok(())
        }
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
}
async fn run_exec_client(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: String,
    args: Vec<String>,
) -> Result<()> {
    let request = ControlRequest::Exec {
        device,
        program,
        args,
        cwd: None,
        run_as,
    };
    match request_control(control, &request).await? {
        ControlResponse::Exec {
            stdout,
            stderr,
            code,
        } => {
            print!("{stdout}");
            eprint!("{stderr}");
            if code.unwrap_or(1) != 0 {
                bail!("remote exit code {:?}", code);
            }
            Ok(())
        }
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
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
    let mut stream = session.stream.lock().await;
    let request = FilePushRequest {
        path: remote,
        identity: run_as.into(),
    };
    write_frame(&mut *stream, &frame(FrameKind::PushRequest, 2, &request)?).await?;
    let ready = read_frame(&mut *stream).await?;
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
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        write_frame(
            &mut *stream,
            &raw_frame(FrameKind::FileChunk, 2, buffer[..n].to_vec()),
        )
        .await?;
    }
    write_frame(&mut *stream, &raw_frame(FrameKind::FileEnd, 2, Vec::new())).await?;
    let done = read_frame(&mut *stream).await?;
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
    let mut stream = session.stream.lock().await;
    let request = FilePullRequest {
        path: remote,
        identity: run_as.into(),
    };
    write_frame(&mut *stream, &frame(FrameKind::PullRequest, 3, &request)?).await?;
    let meta_frame = read_frame(&mut *stream).await?;
    let meta = match meta_frame.kind {
        FrameKind::FileMeta => decode::<FileMeta>(&meta_frame.payload)?,
        FrameKind::Error => bail!(
            "remote pull error: {}",
            String::from_utf8_lossy(&meta_frame.payload)
        ),
        other => bail!("unexpected pull response {other:?}"),
    };
    let path = PathBuf::from(local);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let mut file = File::create(&path)
        .await
        .with_context(|| format!("create {}", path.display()))?;
    let mut written = 0u64;
    loop {
        let incoming = read_frame(&mut *stream).await?;
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
    let mut stream = session.stream.lock().await;
    write_frame(
        &mut *stream,
        &frame(FrameKind::ProxyStartRequest, 4, &request)?,
    )
    .await?;
    let response = read_frame(&mut *stream).await?;
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
            let token = token.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let mut remote = TcpStream::connect((device_ip, proxy_port)).await?;
                    remote.write_all(&token).await?;
                    remote.flush().await?;
                    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
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
            let token = relay_token.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let mut received = vec![0u8; token.len()];
                    incoming.read_exact(&mut received).await?;
                    if received != token {
                        bail!("reverse relay token mismatch");
                    }
                    let mut local = TcpStream::connect(("127.0.0.1", host_port)).await?;
                    tokio::io::copy_bidirectional(&mut incoming, &mut local).await?;
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

fn parse_tcp_spec(value: &str) -> Result<u16> {
    let raw = value.strip_prefix("tcp:").unwrap_or(value);
    raw.parse::<u16>()
        .with_context(|| format!("invalid tcp endpoint '{value}'"))
}

fn absolute_output(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

async fn run_push(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    local: PathBuf,
    remote: String,
) -> Result<()> {
    let local = tokio::fs::canonicalize(local).await?;
    let request = ControlRequest::Push {
        device,
        local: local.to_string_lossy().into_owned(),
        remote,
        run_as,
    };
    match request_control(control, &request).await? {
        ControlResponse::Ok { message } => {
            println!("{message}");
            Ok(())
        }
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
}

async fn run_pull(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    remote: String,
    local: PathBuf,
) -> Result<()> {
    let local = absolute_output(local)?;
    let request = ControlRequest::Pull {
        device,
        remote,
        local: local.to_string_lossy().into_owned(),
        run_as,
    };
    match request_control(control, &request).await? {
        ControlResponse::Ok { message } => {
            println!("{message}");
            Ok(())
        }
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
}

async fn exec_control(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: &str,
    args: Vec<String>,
) -> Result<(String, String, Option<i32>)> {
    let request = ControlRequest::Exec {
        device,
        program: program.into(),
        args,
        cwd: None,
        run_as,
    };
    match request_control(control, &request).await? {
        ControlResponse::Exec {
            stdout,
            stderr,
            code,
        } => Ok((stdout, stderr, code)),
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
}

async fn run_exec_checked(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: &str,
    args: Vec<String>,
) -> Result<()> {
    let (stdout, stderr, code) = exec_control(control, device, run_as, program, args).await?;
    print!("{stdout}");
    eprint!("{stderr}");
    if code.unwrap_or(1) != 0 {
        bail!("remote exit code {:?}", code);
    }
    Ok(())
}

async fn run_install(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    apk: PathBuf,
) -> Result<()> {
    let local = tokio::fs::canonicalize(apk).await?;
    let remote = format!(
        "/data/local/tmp/sidewire-install-{:016x}.apk",
        rand::random::<u64>()
    );
    let push = ControlRequest::Push {
        device: device.clone(),
        local: local.to_string_lossy().into_owned(),
        remote: remote.clone(),
        run_as,
    };
    match request_control(control, &push).await? {
        ControlResponse::Ok { message } => println!("{message}"),
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
    let result = exec_control(
        control,
        device.clone(),
        run_as,
        "/system/bin/pm",
        vec!["install".into(), "-r".into(), remote.clone()],
    )
    .await;
    let _ = exec_control(
        control,
        device,
        run_as,
        "/system/bin/rm",
        vec!["-f".into(), remote],
    )
    .await;
    let (stdout, stderr, code) = result?;
    print!("{stdout}");
    eprint!("{stderr}");
    if code.unwrap_or(1) != 0 {
        bail!("install failed with exit code {:?}", code);
    }
    Ok(())
}

async fn run_logcat(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    clear: bool,
    args: Vec<String>,
) -> Result<()> {
    if clear {
        return run_exec_checked(
            control,
            device,
            run_as,
            "/system/bin/logcat",
            vec!["-c".into()],
        )
        .await;
    }
    run_pty_client(control, device, run_as, "/system/bin/logcat".into(), args).await
}
async fn run_forward(
    control: &str,
    device: Option<String>,
    local: String,
    remote: String,
) -> Result<()> {
    let request = ControlRequest::Forward {
        device,
        local_port: parse_tcp_spec(&local)?,
        remote_port: parse_tcp_spec(&remote)?,
    };
    match request_control(control, &request).await? {
        ControlResponse::Ok { message } => {
            println!("{message}");
            Ok(())
        }
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
}

async fn run_reverse(
    control: &str,
    device: Option<String>,
    remote: String,
    local: String,
) -> Result<()> {
    let request = ControlRequest::Reverse {
        device,
        device_port: parse_tcp_spec(&remote)?,
        host_port: parse_tcp_spec(&local)?,
    };
    match request_control(control, &request).await? {
        ControlResponse::Ok { message } => {
            println!("{message}");
            Ok(())
        }
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
}

async fn run_reboot(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    target: Option<String>,
) -> Result<()> {
    let args = target.into_iter().collect();
    run_exec_checked(control, device, run_as, "/system/bin/reboot", args).await
}

async fn run_screencap(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    output: PathBuf,
) -> Result<()> {
    let remote = format!(
        "/data/local/tmp/sidewire-screencap-{:016x}.png",
        rand::random::<u64>()
    );
    let (_, stderr, code) = exec_control(
        control,
        device.clone(),
        run_as,
        "/system/bin/screencap",
        vec!["-p".into(), remote.clone()],
    )
    .await?;
    if code.unwrap_or(1) != 0 {
        eprint!("{stderr}");
        bail!("screencap failed");
    }
    let local = absolute_output(output)?;
    let pull = ControlRequest::Pull {
        device: device.clone(),
        remote: remote.clone(),
        local: local.to_string_lossy().into_owned(),
        run_as,
    };
    let result = request_control(control, &pull).await?;
    let _ = exec_control(
        control,
        device,
        run_as,
        "/system/bin/rm",
        vec!["-f".into(), remote],
    )
    .await;
    match result {
        ControlResponse::Ok { message } => {
            println!("{message}: {}", local.display());
            Ok(())
        }
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected server response"),
    }
}

async fn run_packages(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    filter: Option<String>,
) -> Result<()> {
    let mut args = vec!["list".into(), "packages".into()];
    if let Some(filter) = filter {
        args.push(filter);
    }
    run_exec_checked(control, device, run_as, "/system/bin/pm", args).await
}

async fn run_app(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    command: AppCommand,
) -> Result<()> {
    match command {
        AppCommand::Start { package } => {
            run_exec_checked(
                control,
                device,
                run_as,
                "/system/bin/monkey",
                vec![
                    "-p".into(),
                    package,
                    "-c".into(),
                    "android.intent.category.LAUNCHER".into(),
                    "1".into(),
                ],
            )
            .await
        }
        AppCommand::Stop { package } => {
            run_exec_checked(
                control,
                device,
                run_as,
                "/system/bin/am",
                vec!["force-stop".into(), package],
            )
            .await
        }
        AppCommand::Clear { package } => {
            run_exec_checked(
                control,
                device,
                run_as,
                "/system/bin/pm",
                vec!["clear".into(), package],
            )
            .await
        }
    }
}

enum ConsoleEvent {
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    Complete {
        line: String,
        cursor: usize,
        reply: std::sync::mpsc::SyncSender<Option<PtyCompleteResult>>,
    },
    Error(String),
    Eof,
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[cfg(not(windows))]
fn key_bytes(key: crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mut bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if c.is_ascii() {
                vec![(c.to_ascii_lowercase() as u8) & 0x1f]
            } else {
                return None;
            }
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        KeyCode::F(5) => b"\x1b[15~".to_vec(),
        KeyCode::F(6) => b"\x1b[17~".to_vec(),
        KeyCode::F(7) => b"\x1b[18~".to_vec(),
        KeyCode::F(8) => b"\x1b[19~".to_vec(),
        KeyCode::F(9) => b"\x1b[20~".to_vec(),
        KeyCode::F(10) => b"\x1b[21~".to_vec(),
        KeyCode::F(11) => b"\x1b[23~".to_vec(),
        KeyCode::F(12) => b"\x1b[24~".to_vec(),
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(not(windows))]
fn spawn_console_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    tokio::task::spawn_blocking(move || {
        use crossterm::event::{Event, poll, read};
        while !stop.load(Ordering::Relaxed) {
            match poll(std::time::Duration::from_millis(50)) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!(
                        "terminal event poll failed: {error}"
                    )));
                    break;
                }
            }
            match read() {
                Ok(Event::Key(key)) => {
                    if let Some(bytes) = key_bytes(key) {
                        if tx.send(ConsoleEvent::Input(bytes)).is_err() {
                            break;
                        }
                    }
                }
                Ok(Event::Resize(cols, rows)) => {
                    if tx.send(ConsoleEvent::Resize { cols, rows }).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!(
                        "terminal event read failed: {error}"
                    )));
                    break;
                }
            }
        }
    });
}

#[cfg(windows)]
fn spawn_console_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    let input_stop = stop.clone();
    let input_tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut byte = [0u8; 1];
        while !input_stop.load(Ordering::Relaxed) {
            match input.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    if input_tx.send(ConsoleEvent::Input(vec![byte[0]])).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ =
                        input_tx.send(ConsoleEvent::Error(format!("stdin read failed: {error}")));
                    break;
                }
            }
        }
    });

    tokio::task::spawn_blocking(move || {
        let mut last = crossterm::terminal::size().unwrap_or((80, 24));
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(size) = crossterm::terminal::size() {
                if size != last {
                    last = size;
                    if tx
                        .send(ConsoleEvent::Resize {
                            cols: size.0,
                            rows: size.1,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
}

async fn run_pty_client(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: String,
    args: Vec<String>,
) -> Result<()> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let request = ControlRequest::Pty {
        device,
        program,
        args,
        run_as,
        cols,
        rows,
        term: "xterm-256color".into(),
        echo: true,
    };
    let mut stream = TcpStream::connect(control)
        .await
        .with_context(|| format!("connect to SideWire server control {control}"))?;
    stream.set_nodelay(true).context("enable TCP_NODELAY")?;
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        bail!("SideWire server closed PTY control connection");
    }
    match serde_json::from_str::<ControlResponse>(line.trim_end())? {
        ControlResponse::Ok { .. } => {}
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected PTY server response"),
    }

    crossterm::terminal::enable_raw_mode().context("enable terminal raw mode")?;
    let raw_guard = RawModeGuard;
    let stop = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_console_reader(stop.clone(), event_tx);
    let stream_id = 0x5054_5901;
    let mut stdout = io::stdout();
    let (mut net_read, mut net_write) = tokio::io::split(reader);
    let input_loop = async {
        loop {
            let event = event_rx
                .recv()
                .await
                .context("terminal input reader stopped")?;
            let outgoing = match event {
                ConsoleEvent::Input(bytes) => raw_frame(FrameKind::PtyInput, stream_id, bytes),
                ConsoleEvent::Resize { cols, rows } => {
                    frame(FrameKind::PtyResize, stream_id, &PtyResize { cols, rows })?
                }
                ConsoleEvent::Complete { reply, .. } => {
                    let _ = reply.send(None);
                    continue;
                }
                ConsoleEvent::Error(message) => {
                    let close = raw_frame(FrameKind::PtyClose, stream_id, Vec::new());
                    let _ = write_frame(&mut net_write, &close).await;
                    bail!(message);
                }
                ConsoleEvent::Eof => {
                    let close = raw_frame(FrameKind::PtyClose, stream_id, Vec::new());
                    write_frame(&mut net_write, &close).await?;
                    return Ok::<(), anyhow::Error>(());
                }
            };
            write_frame(&mut net_write, &outgoing).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };

    let output_loop = async {
        let mut saw_output = false;
        loop {
            let remote = read_frame(&mut net_read).await?;
            match remote.kind {
                FrameKind::PtyOutput => {
                    saw_output = true;
                    stdout.write_all(&remote.payload)?;
                    stdout.flush()?;
                }
                FrameKind::PtyExit => {
                    let exit: PtyExit = decode(&remote.payload)?;
                    if !saw_output || exit.code.unwrap_or(0) != 0 {
                        bail!("remote PTY exited (code {:?})", exit.code);
                    }
                    return Ok::<(), anyhow::Error>(());
                }
                FrameKind::Error => {
                    bail!(
                        "remote PTY error: {}",
                        String::from_utf8_lossy(&remote.payload)
                    );
                }
                kind => bail!("unexpected PTY frame {kind:?}"),
            }
        }
    };

    tokio::pin!(input_loop);
    tokio::pin!(output_loop);
    let result = tokio::select! {
        result = &mut input_loop => result,
        result = &mut output_loop => result,
    };
    stop.store(true, Ordering::Relaxed);
    drop(raw_guard);
    result
}

#[cfg(not(windows))]
fn spawn_line_reader(stop: Arc<AtomicBool>, tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>) {
    spawn_standard_line_reader(stop, tx);
}

fn spawn_standard_line_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    let input_stop = stop.clone();
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut line = String::new();
        while !input_stop.load(Ordering::Relaxed) {
            line.clear();
            match std::io::BufRead::read_line(&mut input, &mut line) {
                Ok(0) => {
                    let _ = input_tx.send(ConsoleEvent::Eof);
                    break;
                }
                Ok(_) => {
                    let bytes = normalize_console_line(line.as_bytes());
                    if input_tx.send(ConsoleEvent::Input(bytes)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = input_tx.send(ConsoleEvent::Error(format!(
                        "stdin line read failed: {error}"
                    )));
                    break;
                }
            }
        }
    });
    spawn_line_resize_reader(stop, tx);
}

fn spawn_line_resize_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    std::thread::spawn(move || {
        let mut last = crossterm::terminal::size().unwrap_or((80, 24));
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Ok(size) = crossterm::terminal::size()
                && size != last
            {
                last = size;
                if tx
                    .send(ConsoleEvent::Resize {
                        cols: size.0,
                        rows: size.1,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    });
}

#[cfg(windows)]
#[repr(C)]
struct ConsoleReadConsoleControl {
    n_length: u32,
    n_initial_chars: u32,
    ctrl_wakeup_mask: u32,
    control_key_state: u32,
}

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    #[link_name = "GetStdHandle"]
    fn get_std_handle(std_handle: u32) -> *mut std::ffi::c_void;
    #[link_name = "GetConsoleMode"]
    fn get_console_mode(console: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    #[link_name = "ReadConsoleW"]
    fn read_console_w(
        console: *mut std::ffi::c_void,
        buffer: *mut std::ffi::c_void,
        chars_to_read: u32,
        chars_read: *mut u32,
        control: *mut ConsoleReadConsoleControl,
    ) -> i32;
    #[link_name = "WriteConsoleW"]
    fn write_console_w(
        console: *mut std::ffi::c_void,
        buffer: *const std::ffi::c_void,
        chars_to_write: u32,
        chars_written: *mut u32,
        reserved: *mut std::ffi::c_void,
    ) -> i32;
}

#[cfg(windows)]
fn windows_console_handle(kind: u32) -> Option<*mut std::ffi::c_void> {
    let handle = unsafe { get_std_handle(kind) };
    let mut mode = 0u32;
    (unsafe { get_console_mode(handle, &mut mode) } != 0).then_some(handle)
}

#[cfg(windows)]
fn write_windows_console(text: &str) -> std::io::Result<()> {
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return Ok(());
    }
    let handle = windows_console_handle(STD_OUTPUT_HANDLE)
        .ok_or_else(|| std::io::Error::other("stdout is not a Windows console"))?;
    let mut written = 0u32;
    let ok = unsafe {
        write_console_w(
            handle,
            wide.as_ptr().cast(),
            wide.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn spawn_line_reader(stop: Arc<AtomicBool>, tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>) {
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const TAB: u16 = 0x09;
    const ERROR_OPERATION_ABORTED: i32 = 995;
    const BUFFER_CHARS: usize = 8192;

    if windows_console_handle(STD_INPUT_HANDLE).is_none() {
        spawn_standard_line_reader(stop, tx);
        return;
    }
    if windows_console_handle(STD_OUTPUT_HANDLE).is_none() {
        spawn_standard_line_reader(stop, tx);
        return;
    }

    let input_stop = stop.clone();
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        let Some(input_handle) = windows_console_handle(STD_INPUT_HANDLE) else {
            let _ = input_tx.send(ConsoleEvent::Error(
                "stdin stopped being a Windows console".into(),
            ));
            return;
        };
        let mut buffer = vec![0u16; BUFFER_CHARS];
        let mut keep = 0usize;
        while !input_stop.load(Ordering::Relaxed) {
            if keep >= buffer.len() - 1 {
                let _ = input_tx.send(ConsoleEvent::Error("console input line is too long".into()));
                break;
            }
            let mut control = ConsoleReadConsoleControl {
                n_length: std::mem::size_of::<ConsoleReadConsoleControl>() as u32,
                n_initial_chars: keep as u32,
                ctrl_wakeup_mask: 1u32 << TAB,
                control_key_state: 0,
            };
            let mut count = 0u32;
            let ok = unsafe {
                read_console_w(
                    input_handle,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut count,
                    &mut control,
                )
            };
            if ok == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_OPERATION_ABORTED) {
                    continue;
                }
                let _ = input_tx.send(ConsoleEvent::Error(format!("ReadConsoleW failed: {error}")));
                break;
            }
            let count = count as usize;
            if count == 0 {
                continue;
            }

            if let Some(tab_index) = buffer[..count]
                .iter()
                .position(|character| *character == TAB)
            {
                let prefix = String::from_utf16_lossy(&buffer[..tab_index]);
                let suffix = String::from_utf16_lossy(&buffer[tab_index + 1..count]);
                let mut line = prefix.clone();
                line.push_str(&suffix);

                if tab_index + 1 != count {
                    let wide: Vec<u16> = line.encode_utf16().collect();
                    if wide.len() >= buffer.len() {
                        let _ = input_tx
                            .send(ConsoleEvent::Error("console input line is too long".into()));
                        break;
                    }
                    buffer[..wide.len()].copy_from_slice(&wide);
                    keep = wide.len();
                    continue;
                }

                let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
                if input_tx
                    .send(ConsoleEvent::Complete {
                        line,
                        cursor: prefix.len(),
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    break;
                }
                let completed = match reply_rx.recv() {
                    Ok(Some(completed)) => completed,
                    Ok(None) | Err(_) => break,
                };
                let completed_cursor = (completed.cursor as usize).min(completed.line.len());
                let new_prefix = completed
                    .line
                    .get(..completed_cursor)
                    .unwrap_or(&completed.line);
                if let Some(addition) = new_prefix.strip_prefix(&prefix)
                    && let Err(error) = write_windows_console(addition)
                {
                    let _ = input_tx.send(ConsoleEvent::Error(format!(
                        "write completion to console failed: {error}"
                    )));
                    break;
                }

                let wide: Vec<u16> = completed.line.encode_utf16().collect();
                if wide.len() >= buffer.len() {
                    let _ = input_tx.send(ConsoleEvent::Error(
                        "completed console line is too long".into(),
                    ));
                    break;
                }
                buffer[..wide.len()].copy_from_slice(&wide);
                keep = wide.len();
                continue;
            }

            let line = String::from_utf16_lossy(&buffer[..count]);
            if input_tx
                .send(ConsoleEvent::Input(normalize_console_line(line.as_bytes())))
                .is_err()
            {
                break;
            }
            keep = 0;
        }
    });

    spawn_line_resize_reader(stop, tx);
}

/// Convert the host console's line ending to one PTY newline.
///
/// Windows cooked console input ends submitted lines with CRLF. Forwarding
/// both bytes to an Android PTY with ICRNL enabled turns them into two newline
/// characters, so mksh executes an extra empty command and prints two prompts.
fn normalize_console_line(line: &[u8]) -> Vec<u8> {
    let Some(without_lf) = line.strip_suffix(b"\n") else {
        return line.to_vec();
    };
    let content = without_lf.strip_suffix(b"\r").unwrap_or(without_lf);
    let mut normalized = Vec::with_capacity(content.len() + 1);
    normalized.extend_from_slice(content);
    normalized.push(b'\n');
    normalized
}

async fn open_pty_control(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: String,
    args: Vec<String>,
    echo: bool,
) -> Result<BufReader<TcpStream>> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let request = ControlRequest::Pty {
        device,
        program,
        args,
        run_as,
        cols,
        rows,
        term: "xterm-256color".into(),
        echo,
    };
    let mut stream = TcpStream::connect(control)
        .await
        .with_context(|| format!("connect to SideWire server control {control}"))?;
    stream.set_nodelay(true).context("enable TCP_NODELAY")?;
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        bail!("SideWire server closed PTY control connection");
    }
    match serde_json::from_str::<ControlResponse>(line.trim_end())? {
        ControlResponse::Ok { .. } => Ok(reader),
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected PTY server response"),
    }
}

async fn run_pty_line_client(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: String,
    args: Vec<String>,
) -> Result<()> {
    let reader = open_pty_control(control, device, run_as, program, args, false).await?;
    let stream_id = 0x5054_5901;
    let stop = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_line_reader(stop.clone(), event_tx);
    let (mut net_read, mut net_write) = tokio::io::split(reader);
    let (remote_tx, mut remote_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match read_frame(&mut net_read).await {
                Ok(frame) => {
                    if remote_tx.send(Ok(frame)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = remote_tx.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    let mut stdout = io::stdout();
    let mut completion_reply: Option<std::sync::mpsc::SyncSender<Option<PtyCompleteResult>>> = None;
    let result = loop {
        tokio::select! {
            event = event_rx.recv() => match event {
                Some(ConsoleEvent::Input(bytes)) => {
                    write_frame(&mut net_write, &raw_frame(FrameKind::PtyInput, stream_id, bytes)).await?;
                }
                Some(ConsoleEvent::Resize { cols, rows }) => {
                    let resize = frame(FrameKind::PtyResize, stream_id, &PtyResize { cols, rows })?;
                    write_frame(&mut net_write, &resize).await?;
                }
                Some(ConsoleEvent::Complete { line, cursor, reply }) => {
                    if completion_reply.is_some() {
                        let _ = reply.send(None);
                        continue;
                    }
                    let request = PtyCompleteRequest {
                        line,
                        cursor: cursor as u32,
                    };
                    write_frame(
                        &mut net_write,
                        &frame(FrameKind::PtyComplete, stream_id, &request)?,
                    )
                    .await?;
                    completion_reply = Some(reply);
                }
                Some(ConsoleEvent::Error(message)) => break Err(anyhow::anyhow!(message)),
                Some(ConsoleEvent::Eof) | None => {
                    let close = raw_frame(FrameKind::PtyClose, stream_id, Vec::new());
                    let _ = write_frame(&mut net_write, &close).await;
                    break Ok(());
                }
            },
            signal = tokio::signal::ctrl_c() => {
                signal.context("wait for Ctrl+C")?;
                write_frame(&mut net_write, &raw_frame(FrameKind::PtyInput, stream_id, vec![0x03])).await?;
            }
            remote = remote_rx.recv() => match remote {
                Some(Ok(frame)) => match frame.kind {
                    FrameKind::PtyOutput => { stdout.write_all(&frame.payload)?; stdout.flush()?; }
                    FrameKind::PtyCompleteResult => {
                        let completion: PtyCompleteResult = decode(&frame.payload)?;
                        if let Some(reply) = completion_reply.take() {
                            let _ = reply.send(Some(completion));
                        }
                    }
                    FrameKind::PtyExit => break Ok(()),
                    FrameKind::Error => break Err(anyhow::anyhow!("remote PTY error: {}", String::from_utf8_lossy(&frame.payload))),
                    kind => break Err(anyhow::anyhow!("unexpected PTY frame {kind:?}")),
                },
                Some(Err(message)) => break Err(anyhow::anyhow!(message)),
                None => break Err(anyhow::anyhow!("PTY network reader stopped")),
            }
        }
    };
    if let Some(reply) = completion_reply.take() {
        let _ = reply.send(None);
    }
    stop.store(true, Ordering::Relaxed);
    result
}

async fn run_pty_probe(control: &str, device: Option<String>, run_as: RunAs) -> Result<()> {
    let mut stream = open_pty_control(
        control,
        device,
        run_as,
        "/system/bin/sh".into(),
        Vec::new(),
        false,
    )
    .await?;
    let stream_id = 0x5054_5901;
    let command = b"printf '__SIDEWIRE_PTY_OK__\\n'; id; id -Z; tty; exit\r".to_vec();
    write_frame(
        &mut stream,
        &raw_frame(FrameKind::PtyInput, stream_id, command),
    )
    .await?;
    let probe = async {
        let mut output = Vec::new();
        loop {
            let frame = read_frame(&mut stream).await?;
            match frame.kind {
                FrameKind::PtyOutput => {
                    io::stdout().write_all(&frame.payload)?;
                    io::stdout().flush()?;
                    output.extend_from_slice(&frame.payload);
                }
                FrameKind::PtyExit => break,
                FrameKind::Error => bail!(
                    "remote PTY error: {}",
                    String::from_utf8_lossy(&frame.payload)
                ),
                kind => bail!("unexpected PTY frame {kind:?}"),
            }
        }
        if !output
            .windows(b"__SIDEWIRE_PTY_OK__".len())
            .any(|w| w == b"__SIDEWIRE_PTY_OK__")
        {
            bail!("PTY probe completed without input marker");
        }
        Ok::<(), anyhow::Error>(())
    };
    match tokio::time::timeout(tokio::time::Duration::from_secs(6), probe).await {
        Ok(result) => result,
        Err(_) => bail!("PTY probe timed out: remote shell did not consume synthetic input"),
    }
}

async fn run_shell(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    raw: bool,
    probe: bool,
) -> Result<()> {
    if probe {
        return run_pty_probe(control, device, run_as).await;
    }
    if raw {
        run_pty_client(control, device, run_as, "/system/bin/sh".into(), Vec::new()).await
    } else {
        // The Windows console already echoes and edits cooked line input. Turn
        // off mksh's remote editor so the submitted command is not drawn a
        // second time; raw mode keeps it enabled for history and completion.
        run_pty_line_client(
            control,
            device,
            run_as,
            "/system/bin/sh".into(),
            vec!["+o".into(), "emacs".into()],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_console_line;

    #[test]
    fn normalizes_windows_console_crlf_to_one_pty_enter() {
        assert_eq!(normalize_console_line(b"ls\r\n"), b"ls\n");
        assert_eq!(normalize_console_line(b"\r\n"), b"\n");
    }

    #[test]
    fn normalizes_unix_console_lf_to_one_pty_enter() {
        assert_eq!(normalize_console_line(b"pwd\n"), b"pwd\n");
        assert_eq!(normalize_console_line(b"\n"), b"\n");
    }

    #[test]
    fn leaves_unterminated_input_unchanged() {
        assert_eq!(normalize_console_line(b"exit"), b"exit");
    }
}
