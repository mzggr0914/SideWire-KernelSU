use super::*;

#[cfg(windows)]
pub(super) type ControlStream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
pub(super) type ControlStream = tokio::net::UnixStream;

#[cfg(windows)]
pub(super) fn default_endpoint() -> Result<String> {
    Ok(r"\\.\pipe\sidewire".to_owned())
}

#[cfg(unix)]
pub(super) fn default_endpoint() -> Result<String> {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Ok(std::path::PathBuf::from(runtime)
            .join("sidewire.sock")
            .to_string_lossy()
            .into_owned());
    }
    let config = crate::config::config_path()?;
    let parent = config
        .parent()
        .context("SideWire config path has no parent")?;
    Ok(parent.join("sidewire.sock").to_string_lossy().into_owned())
}

pub(super) fn resolve_endpoint(requested: Option<String>) -> Result<String> {
    requested.map_or_else(default_endpoint, Ok)
}
#[cfg(windows)]
pub(super) async fn connect(endpoint: &str) -> Result<ControlStream> {
    use std::io::ErrorKind;
    use tokio::net::windows::named_pipe::ClientOptions;

    let started = tokio::time::Instant::now();
    loop {
        match ClientOptions::new().open(endpoint) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::WouldBlock)
                    && started.elapsed() < tokio::time::Duration::from_secs(2) =>
            {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("connect SideWire control pipe {endpoint}"));
            }
        }
    }
}

#[cfg(unix)]
pub(super) async fn connect(endpoint: &str) -> Result<ControlStream> {
    tokio::net::UnixStream::connect(endpoint)
        .await
        .with_context(|| format!("connect SideWire control socket {endpoint}"))
}

#[cfg(windows)]
pub(super) fn kind() -> &'static str {
    "named-pipe"
}
#[cfg(unix)]
pub(super) fn kind() -> &'static str {
    "unix-socket"
}
