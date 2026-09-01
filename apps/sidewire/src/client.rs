use super::*;

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

pub(super) async fn run_devices(control: &str) -> Result<()> {
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
pub(super) async fn run_exec_client(
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

pub(super) async fn run_push(
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

pub(super) async fn run_pull(
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

pub(super) async fn run_exec_checked(
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

pub(super) async fn run_install(
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

pub(super) async fn run_logcat(
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
    pty::run_pty_client(control, device, run_as, "/system/bin/logcat".into(), args).await
}
pub(super) async fn run_forward(
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

pub(super) async fn run_reverse(
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

pub(super) async fn run_reboot(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    target: Option<String>,
) -> Result<()> {
    let args = target.into_iter().collect();
    run_exec_checked(control, device, run_as, "/system/bin/reboot", args).await
}

pub(super) async fn run_screencap(
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

pub(super) async fn run_packages(
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

pub(super) async fn run_app(
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
