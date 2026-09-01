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

mod client;
mod pty;
mod server;

use server::{ControlRequest, ControlResponse};

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    match Cli::parse().command {
        Command::Server { bind, control } => server::run_server(&bind, &control).await,
        Command::Devices { control } => client::run_devices(&control).await,
        Command::Exec {
            control,
            device,
            run_as,
            program,
            args,
        } => client::run_exec_client(&control, device, run_as, program, args).await,
        Command::Shell {
            control,
            device,
            run_as,
            raw,
            probe,
        } => pty::run_shell(&control, device, run_as, raw, probe).await,
        Command::Push {
            control,
            device,
            run_as,
            local,
            remote,
        } => client::run_push(&control, device, run_as, local, remote).await,
        Command::Pull {
            control,
            device,
            run_as,
            remote,
            local,
        } => client::run_pull(&control, device, run_as, remote, local).await,
        Command::Install {
            control,
            device,
            run_as,
            apk,
        } => client::run_install(&control, device, run_as, apk).await,
        Command::Uninstall {
            control,
            device,
            run_as,
            package,
        } => {
            client::run_exec_checked(
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
        } => client::run_logcat(&control, device, run_as, clear, args).await,
        Command::Forward {
            control,
            device,
            local,
            remote,
        } => client::run_forward(&control, device, local, remote).await,
        Command::Reverse {
            control,
            device,
            remote,
            local,
        } => client::run_reverse(&control, device, remote, local).await,
        Command::Reboot {
            control,
            device,
            run_as,
            target,
        } => client::run_reboot(&control, device, run_as, target).await,
        Command::Screencap {
            control,
            device,
            run_as,
            output,
        } => client::run_screencap(&control, device, run_as, output).await,
        Command::Packages {
            control,
            device,
            run_as,
            filter,
        } => client::run_packages(&control, device, run_as, filter).await,
        Command::App {
            control,
            device,
            run_as,
            command,
        } => client::run_app(&control, device, run_as, command).await,
    }
}
