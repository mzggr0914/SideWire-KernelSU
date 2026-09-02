use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sidewire_protocol::{
    ClipboardData, ClipboardSetRequest, ExecExit, ExecIdentity, ExecRequest, FileMeta,
    FilePullRequest, FilePushRequest, FrameKind, Hello, HelloAck, ProxyStartAck, ProxyStartRequest,
    ProxyTokenMode, PtyExit, PtyOpenAck, PtyOpenRequest, PtyResize, decode, frame, raw_frame,
    read_frame, write_frame, write_raw_frame,
};
#[cfg(windows)]
use sidewire_protocol::{PtyCompleteRequest, PtyCompleteResult};
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
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::RwLock,
};

const DEFAULT_DEVICE_BIND: &str = "0.0.0.0:58321";

mod client;
mod config;
mod control;
mod discovery;
mod pairing;
mod pty;
mod server;
mod transfer;
mod transport;
mod trust;

use server::{ControlRequest, ControlResponse};

#[derive(Clone, Copy, Debug, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
    /// Override the local control IPC endpoint (named pipe on Windows, Unix socket on Unix).
    #[arg(long, global = true, value_name = "PIPE|SOCKET")]
    control: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Server {
        #[arg(long, default_value = DEFAULT_DEVICE_BIND)]
        bind: String,
        /// Connect to an Android daemon running in inbound mode.
        #[arg(long = "connect", value_name = "HOST:PORT")]
        connect: Vec<String>,
        /// Discover inbound SideWire devices on the local network.
        #[arg(long)]
        discover: bool,
        /// Disable authentication and encryption. Both sides must explicitly use insecure mode.
        #[arg(long)]
        insecure: bool,
    },
    Pair {
        /// Android IP or pairing endpoint. Omit with --discover.
        target: Option<String>,
        #[arg(long)]
        discover: bool,
    },
    Paired,
    Unpair {
        device: String,
    },
    Discover {
        #[arg(long, default_value_t = 1500)]
        timeout_ms: u64,
    },
    Doctor {
        #[arg(short = 's', long)]
        device: Option<String>,
    },
    WaitForDevice {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Devices {},
    Exec {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        program: String,
        args: Vec<String>,
    },
    Shell {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        /// Use per-keystroke raw console input. Default is reliable line input.
        #[arg(long)]
        raw: bool,
        /// Send a synthetic command sequence to verify the PTY path.
        #[arg(long)]
        probe: bool,
    },
    Push {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        local: PathBuf,
        remote: String,
    },
    Pull {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        remote: String,
        local: PathBuf,
    },
    Install {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        apk: PathBuf,
    },
    Uninstall {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        package: String,
    },
    Logcat {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        #[arg(long)]
        clear: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Forward {
        #[arg(short = 's', long)]
        device: Option<String>,
        local: String,
        remote: String,
    },
    Reverse {
        #[arg(short = 's', long)]
        device: Option<String>,
        remote: String,
        local: String,
    },
    Reboot {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        target: Option<String>,
    },
    Screencap {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        output: PathBuf,
    },
    Packages {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        filter: Option<String>,
    },
    Clipboard {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[command(subcommand)]
        command: ClipboardCommand,
    },
    App {
        #[arg(short = 's', long)]
        device: Option<String>,
        #[arg(long = "as", value_enum)]
        run_as: Option<RunAs>,
        #[command(subcommand)]
        command: AppCommand,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    Show,
    Set { key: String, value: String },
    Unset { key: String },
}

#[derive(Subcommand)]
enum ClipboardCommand {
    /// Print the Android clipboard text.
    Get,
    /// Set Android clipboard text directly.
    Set { text: String },
    /// Copy the current PC clipboard to Android.
    Push,
    /// Copy the current Android clipboard to the PC clipboard.
    Pull,
    /// Clear the Android clipboard.
    Clear,
}

#[derive(Subcommand)]
enum AppCommand {
    Start { package: String },
    Stop { package: String },
    Clear { package: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = Cli::parse();
    let control = control::resolve_endpoint(cli.control)?;
    let app_config = config::load()?;
    match cli.command {
        Command::Server {
            bind,
            mut connect,
            discover,
            insecure,
        } => {
            if connect.is_empty()
                && let Some(endpoint) = app_config.connect.clone()
            {
                connect.push(endpoint);
            }
            server::run_server(&bind, &control, connect, discover, insecure).await
        }
        Command::Pair { target, discover } => pairing::run_pair(target, discover).await,
        Command::Paired => pairing::run_paired(),
        Command::Unpair { device } => pairing::run_unpair(device),
        Command::Discover { timeout_ms } => discovery::run_discover(timeout_ms).await,
        Command::Doctor { device } => {
            client::run_doctor(
                &control,
                config::resolve_device(&app_config, device),
                &app_config,
            )
            .await
        }
        Command::WaitForDevice { device, timeout } => {
            client::run_wait_for_device(
                &control,
                config::resolve_device(&app_config, device),
                timeout,
            )
            .await
        }
        Command::Config { command } => match command {
            ConfigCommand::Show => config::show(),
            ConfigCommand::Set { key, value } => config::set(&key, &value),
            ConfigCommand::Unset { key } => config::unset(&key),
        },
        Command::Devices {} => client::run_devices(&control, &app_config).await,
        Command::Exec {
            device,
            all,
            run_as,
            program,
            args,
        } => {
            if all && device.is_some() {
                bail!("--all cannot be combined with -s/--device");
            }
            let run_as = config::resolve_run_as(&app_config, run_as, RunAs::Shell);
            if all {
                client::run_exec_all(&control, run_as, program, args).await
            } else {
                client::run_exec_client(
                    &control,
                    config::resolve_device(&app_config, device),
                    run_as,
                    program,
                    args,
                )
                .await
            }
        }
        Command::Shell {
            device,
            run_as,
            raw,
            probe,
        } => {
            pty::run_shell(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                raw,
                probe,
            )
            .await
        }
        Command::Push {
            device,
            all,
            run_as,
            local,
            remote,
        } => {
            if all && device.is_some() {
                bail!("--all cannot be combined with -s/--device");
            }
            let run_as = config::resolve_run_as(&app_config, run_as, RunAs::Shell);
            if all {
                transfer::run_push_all(&control, run_as, local, remote).await
            } else {
                transfer::run_push(
                    &control,
                    config::resolve_device(&app_config, device),
                    run_as,
                    local,
                    remote,
                )
                .await
            }
        }
        Command::Pull {
            device,
            run_as,
            remote,
            local,
        } => {
            transfer::run_pull(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                remote,
                local,
            )
            .await
        }
        Command::Install {
            device,
            run_as,
            apk,
        } => {
            client::run_install(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                apk,
            )
            .await
        }
        Command::Uninstall {
            device,
            run_as,
            package,
        } => {
            client::run_exec_checked(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                "/system/bin/pm",
                vec!["uninstall".into(), package],
            )
            .await
        }
        Command::Logcat {
            device,
            run_as,
            clear,
            args,
        } => {
            client::run_logcat(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                clear,
                args,
            )
            .await
        }
        Command::Forward {
            device,
            local,
            remote,
        } => {
            client::run_forward(
                &control,
                config::resolve_device(&app_config, device),
                local,
                remote,
            )
            .await
        }
        Command::Reverse {
            device,
            remote,
            local,
        } => {
            client::run_reverse(
                &control,
                config::resolve_device(&app_config, device),
                remote,
                local,
            )
            .await
        }
        Command::Reboot {
            device,
            run_as,
            target,
        } => {
            client::run_reboot(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Root),
                target,
            )
            .await
        }
        Command::Screencap {
            device,
            run_as,
            output,
        } => {
            client::run_screencap(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                output,
            )
            .await
        }
        Command::Packages {
            device,
            run_as,
            filter,
        } => {
            client::run_packages(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                filter,
            )
            .await
        }
        Command::Clipboard { device, command } => {
            client::run_clipboard(
                &control,
                config::resolve_device(&app_config, device),
                command,
            )
            .await
        }
        Command::App {
            device,
            run_as,
            command,
        } => {
            client::run_app(
                &control,
                config::resolve_device(&app_config, device),
                config::resolve_run_as(&app_config, run_as, RunAs::Shell),
                command,
            )
            .await
        }
    }
}
