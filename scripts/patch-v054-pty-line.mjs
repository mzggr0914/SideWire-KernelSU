import fs from "node:fs";

const read = (p) => fs.readFileSync(p, "utf8");
const write = (p, s) => fs.writeFileSync(p, s);
const replaceOnce = (s, oldText, newText, label) => {
  if (!s.includes(oldText)) throw new Error(`missing ${label}`);
  return s.replace(oldText, newText);
};

let proto = read("crates/sidewire-protocol/src/lib.rs");
proto = replaceOnce(proto, "pub const VERSION: u16 = 3;", "pub const VERSION: u16 = 4;", "protocol version");
proto = replaceOnce(proto,
`    pub rows: u16,
    pub term: String,
}`,
`    pub rows: u16,
    pub term: String,
    pub echo: bool,
}`,
"PTY echo field");
write("crates/sidewire-protocol/src/lib.rs", proto);

let cli = read("apps/sidewire/src/main.rs");
cli = replaceOnce(cli,
`        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
    },
    Push {`,
`        #[arg(long = "as", value_enum, default_value = "shell")]
        run_as: RunAs,
        /// Use per-keystroke raw console input. Default is reliable line input.
        #[arg(long)]
        raw: bool,
        /// Send a synthetic command sequence to verify the PTY path.
        #[arg(long)]
        probe: bool,
    },
    Push {`,
"shell flags");
cli = replaceOnce(cli,
`        rows: u16,
        term: String,
    },`,
`        rows: u16,
        term: String,
        echo: bool,
    },`,
"control PTY echo");
write("apps/sidewire/src/main.rs", cli);
cli = read("apps/sidewire/src/main.rs");
cli = replaceOnce(cli,
`        Command::Shell {
            control,
            device,
            run_as,
        } => run_shell(&control, device, run_as).await,`,
`        Command::Shell {
            control,
            device,
            run_as,
            raw,
            probe,
        } => run_shell(&control, device, run_as, raw, probe).await,`,
"main shell dispatch");
cli = replaceOnce(cli,
`            rows,
            term,
        } => {
            handle_control_pty(
                reader, &devices, device, program, args, run_as, cols, rows, term,
            )`,
`            rows,
            term,
            echo,
        } => {
            handle_control_pty(
                reader, &devices, device, program, args, run_as, cols, rows, term, echo,
            )`,
"control PTY dispatch");
cli = replaceOnce(cli,
`    rows: u16,
    term: String,
) -> Result<()> {
    let (_, session) = resolve_device(devices, device.as_deref()).await?;`,
`    rows: u16,
    term: String,
    echo: bool,
) -> Result<()> {
    let (device_name, session) = resolve_device(devices, device.as_deref()).await?;`,
"PTY handler signature");
cli = replaceOnce(cli,
`        rows,
        term,
    };`,
`        rows,
        term,
        echo,
    };`,
"PTY request echo");
write("apps/sidewire/src/main.rs", cli);
cli = read("apps/sidewire/src/main.rs");
const hStart = cli.indexOf("async fn handle_control_pty(\n");
const hEnd = cli.indexOf("async fn process_control(", hStart);
if (hStart < 0 || hEnd < 0) throw new Error("PTY handler block not found");
const handler = String.raw`async fn handle_control_pty(
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
    write_frame(&mut *device_stream, &frame(FrameKind::PtyOpen, stream_id, &request)?).await?;
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
                        FrameKind::PtyInput | FrameKind::PtyResize | FrameKind::PtyClose => {
                            local.stream_id = stream_id;
                            write_frame(&mut device_write, &local).await?;
                            if local.kind == FrameKind::PtyClose { return Ok::<(), anyhow::Error>(()); }
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
                if done { return Ok::<(), anyhow::Error>(()); }
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

`;
cli = cli.slice(0, hStart) + handler + cli.slice(hEnd);
write("apps/sidewire/src/main.rs", cli);
cli = read("apps/sidewire/src/main.rs");
cli = replaceOnce(cli,
`enum ConsoleEvent {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Error(String),
}`,
`enum ConsoleEvent {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Error(String),
    Eof,
}`,
"console EOF event");
cli = replaceOnce(cli,
`        rows,
        term: "xterm-256color".into(),
    };`,
`        rows,
        term: "xterm-256color".into(),
        echo: true,
    };`,
"raw PTY echo");
cli = replaceOnce(cli,
`                ConsoleEvent::Error(message) => {
                    let close = raw_frame(FrameKind::PtyClose, stream_id, Vec::new());
                    let _ = write_frame(&mut net_write, &close).await;
                    bail!(message);
                }
            };`,
`                ConsoleEvent::Error(message) => {
                    let close = raw_frame(FrameKind::PtyClose, stream_id, Vec::new());
                    let _ = write_frame(&mut net_write, &close).await;
                    bail!(message);
                }
                ConsoleEvent::Eof => {
                    let close = raw_frame(FrameKind::PtyClose, stream_id, Vec::new());
                    write_frame(&mut net_write, &close).await?;
                    return Ok::<(), anyhow::Error>(());
                }
            };`,
"raw EOF handling");
write("apps/sidewire/src/main.rs", cli);
cli = read("apps/sidewire/src/main.rs");
const shellPos = cli.indexOf("async fn run_shell(");
if (shellPos < 0) throw new Error("run_shell marker missing");
const shellEnd = cli.indexOf("\n}", shellPos) + 2;
const lineFunctions = String.raw`fn spawn_line_reader(
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
                Ok(0) => { let _ = input_tx.send(ConsoleEvent::Eof); break; }
                Ok(_) => {
                    if input_tx.send(ConsoleEvent::Input(line.as_bytes().to_vec())).is_err() { break; }
                }
                Err(error) => {
                    let _ = input_tx.send(ConsoleEvent::Error(format!("stdin line read failed: {error}")));
                    break;
                }
            }
        }
    });
    std::thread::spawn(move || {
        let mut last = crossterm::terminal::size().unwrap_or((80, 24));
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Ok(size) = crossterm::terminal::size() {
                if size != last {
                    last = size;
                    if tx.send(ConsoleEvent::Resize { cols: size.0, rows: size.1 }).is_err() { break; }
                }
            }
        }
    });
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
        device, program, args, run_as, cols, rows,
        term: "xterm-256color".into(), echo,
    };
    let mut stream = TcpStream::connect(control)
        .await
        .with_context(|| format!("connect to SideWire server control {control}"))?;
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() { bail!("SideWire server closed PTY control connection"); }
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
                Ok(frame) => { if remote_tx.send(Ok(frame)).is_err() { break; } }
                Err(error) => { let _ = remote_tx.send(Err(error.to_string())); break; }
            }
        }
    });
    let mut stdout = io::stdout();
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
                    FrameKind::PtyExit => break Ok(()),
                    FrameKind::Error => break Err(anyhow::anyhow!("remote PTY error: {}", String::from_utf8_lossy(&frame.payload))),
                    kind => break Err(anyhow::anyhow!("unexpected PTY frame {kind:?}")),
                },
                Some(Err(message)) => break Err(anyhow::anyhow!(message)),
                None => break Err(anyhow::anyhow!("PTY network reader stopped")),
            }
        }
    };
    stop.store(true, Ordering::Relaxed);
    result
}

async fn run_pty_probe(control: &str, device: Option<String>, run_as: RunAs) -> Result<()> {
    let mut stream = open_pty_control(
        control, device, run_as, "/system/bin/sh".into(), Vec::new(), false,
    ).await?;
    let stream_id = 0x5054_5901;
    let command = b"printf '__SIDEWIRE_PTY_OK__\\n'; id; id -Z; tty; exit\r".to_vec();
    write_frame(&mut stream, &raw_frame(FrameKind::PtyInput, stream_id, command)).await?;
    let probe = async {
        let mut output = Vec::new();
        loop {
            let frame = read_frame(&mut stream).await?;
            match frame.kind {
                FrameKind::PtyOutput => { io::stdout().write_all(&frame.payload)?; io::stdout().flush()?; output.extend_from_slice(&frame.payload); }
                FrameKind::PtyExit => break,
                FrameKind::Error => bail!("remote PTY error: {}", String::from_utf8_lossy(&frame.payload)),
                kind => bail!("unexpected PTY frame {kind:?}"),
            }
        }
        if !output.windows(b"__SIDEWIRE_PTY_OK__".len()).any(|w| w == b"__SIDEWIRE_PTY_OK__") {
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
    if probe { return run_pty_probe(control, device, run_as).await; }
    if raw {
        run_pty_client(control, device, run_as, "/system/bin/sh".into(), Vec::new()).await
    } else {
        run_pty_line_client(control, device, run_as, "/system/bin/sh".into(), Vec::new()).await
    }
}`;
cli = cli.slice(0, shellPos) + lineFunctions + cli.slice(shellEnd);
write("apps/sidewire/src/main.rs", cli);
let daemon = read("apps/sidewired/src/main.rs");
daemon = replaceOnce(daemon,
`fn configure_pty_child(slave_name: &std::ffi::CStr, identity: ExecIdentity) -> std::io::Result<()> {`,
`fn configure_pty_child(
    slave_name: &std::ffi::CStr,
    identity: ExecIdentity,
    echo: bool,
) -> std::io::Result<()> {`,
"PTY child signature");
daemon = replaceOnce(daemon,
`    if slave_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) } < 0 {`,
`    if slave_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(slave_fd, &mut attrs) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(slave_fd); }
        return Err(error);
    }
    if echo {
        attrs.c_lflag |= libc::ECHO;
    } else {
        attrs.c_lflag &= !(libc::ECHO | libc::ECHONL);
    }
    if unsafe { libc::tcsetattr(slave_fd, libc::TCSANOW, &attrs) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(slave_fd); }
        return Err(error);
    }
    if unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) } < 0 {`,
"PTY echo setup");
daemon = replaceOnce(daemon,
`        let identity = request.identity;
        unsafe {
            command.pre_exec(move || configure_pty_child(&slave_name, identity));
        }`,
`        let identity = request.identity;
        let echo = request.echo;
        unsafe {
            command.pre_exec(move || configure_pty_child(&slave_name, identity, echo));
        }`,
"PTY echo pass-through");
write("apps/sidewired/src/main.rs", daemon);
daemon = read("apps/sidewired/src/main.rs");
daemon = replaceOnce(daemon,
`    apply_identity_now(identity)?;
    Ok(())
}
async fn handle_pty`,
`    apply_identity_now(identity)?;
    Ok(())
}

#[cfg(target_os = "android")]
fn terminate_pty_group(pid: u32) {
    if pid == 0 { return; }
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGHUP);
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

async fn handle_pty`,
"PTY group terminator");
daemon = daemon.replaceAll(
`let _ = child.start_kill();`,
`terminate_pty_group(pid);
                                let _ = child.start_kill();`
);
const outputWrite = `                        write_frame(
                            &mut net_write,
                            &raw_frame(FrameKind::PtyOutput, stream_id, buffer[..n].to_vec()),
                        )
                        .await?;`;
const outputWriteSafe = `                        if let Err(error) = write_frame(
                            &mut net_write,
                            &raw_frame(FrameKind::PtyOutput, stream_id, buffer[..n].to_vec()),
                        )
                        .await {
                            terminate_pty_group(pid);
                            return Err(error).context("write PTY output");
                        }`;
daemon = replaceOnce(daemon, outputWrite, outputWriteSafe, "PTY output disconnect cleanup");
write("apps/sidewired/src/main.rs", daemon);

let cargo = read("Cargo.toml");
cargo = replaceOnce(cargo, `version = "0.5.2"`, `version = "0.5.4"`, "workspace version");
write("Cargo.toml", cargo);
let prop = read("module/module.prop");
prop = prop.replace(/version=0\.5\.2/, "version=0.5.4").replace(/versionCode=9/, "versionCode=10");
prop = prop.replace(
  "description=Native Android bridge with real PTY shell, root/shell execution, streaming logcat, file transfer, app tools, and TCP forwarding.",
  "description=Native Android bridge with reliable PTY line shell, optional raw input, PTY probe, root/shell execution, file transfer, app tools, and TCP forwarding."
);
write("module/module.prop", prop);
let customize = read("module/customize.sh").replace(/SideWire 0\.5\.2/g, "SideWire 0.5.4");
write("module/customize.sh", customize);

console.log("v0.5.4 PTY line-mode patch applied");
