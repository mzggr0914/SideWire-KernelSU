use anyhow::{Context, Result, bail};
use sidewire_protocol::ExecIdentity;
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout},
    sync::{Mutex, watch},
    task::JoinHandle,
};

use super::{command_for_identity, wait_for_stream_cancel};

const OP_GET: u8 = 1;
const OP_SET: u8 = 2;
const OP_CLEAR: u8 = 3;
const STATUS_OK: u8 = 0;
const STATUS_ERROR: u8 = 1;
const MAX_CLIPBOARD_TEXT: usize = 4 * 1024 * 1024;
const HELPER_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type SharedClipboardHelper = Arc<Mutex<ClipboardHelperManager>>;

struct ClipboardHelperProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr_task: JoinHandle<()>,
}

pub(crate) struct ClipboardHelperManager {
    helper: String,
    process: Option<ClipboardHelperProcess>,
}

async fn exchange_io<R, W>(
    reader: &mut R,
    writer: &mut W,
    opcode: u8,
    payload: &[u8],
) -> Result<(u8, Vec<u8>)>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_CLIPBOARD_TEXT {
        bail!("clipboard text exceeds 4 MiB limit");
    }
    let mut header = [0u8; 5];
    header[0] = opcode;
    header[1..].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;

    let mut response_header = [0u8; 5];
    reader.read_exact(&mut response_header).await?;
    let status = response_header[0];
    let length = u32::from_be_bytes(response_header[1..].try_into().unwrap()) as usize;
    if length > MAX_CLIPBOARD_TEXT {
        bail!("clipboard helper response exceeds 4 MiB limit");
    }
    let mut response = vec![0u8; length];
    reader.read_exact(&mut response).await?;
    Ok((status, response))
}
impl ClipboardHelperManager {
    pub(crate) fn shared(helper: String) -> Option<SharedClipboardHelper> {
        Path::new(&helper).is_file().then(|| {
            Arc::new(Mutex::new(Self {
                helper,
                process: None,
            }))
        })
    }

    async fn start(&self) -> Result<ClipboardHelperProcess> {
        let args = vec![
            "/system/bin".to_owned(),
            "com.sidewire.ClipboardHelper".to_owned(),
            "server".to_owned(),
        ];
        let mut command =
            command_for_identity("/system/bin/app_process", &args, ExecIdentity::Shell);
        command
            .env("CLASSPATH", &self.helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().context("start Android clipboard helper")?;
        let stdin = child.stdin.take().context("open clipboard helper stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("open clipboard helper stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("open clipboard helper stderr")?;
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut buffer = [0u8; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(size) => tracing::debug!(
                        message = %String::from_utf8_lossy(&buffer[..size]).trim_end(),
                        "clipboard helper stderr"
                    ),
                    Err(error) => {
                        tracing::debug!(%error, "clipboard helper stderr closed");
                        break;
                    }
                }
            }
        });
        tracing::debug!("started persistent Android clipboard helper");
        Ok(ClipboardHelperProcess {
            child,
            stdin,
            stdout,
            stderr_task,
        })
    }

    async fn stop_process(mut process: ClipboardHelperProcess) {
        let _ = process.child.start_kill();
        let _ = process.child.wait().await;
        process.stderr_task.abort();
    }
    async fn exchange(
        process: &mut ClipboardHelperProcess,
        opcode: u8,
        payload: &[u8],
    ) -> Result<(u8, Vec<u8>)> {
        exchange_io(&mut process.stdout, &mut process.stdin, opcode, payload).await
    }

    async fn request(
        &mut self,
        opcode: u8,
        payload: &[u8],
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<Vec<u8>> {
        if *cancel.borrow() {
            bail!("clipboard stream canceled");
        }
        if self.process.is_none() {
            self.process = Some(self.start().await?);
        }
        let mut process = self.process.take().expect("clipboard helper started");
        if let Some(status) = process.child.try_wait().context("check clipboard helper")? {
            process.stderr_task.abort();
            tracing::debug!(%status, "clipboard helper exited; restarting");
            process = self.start().await?;
        }

        let response = {
            let exchange = Self::exchange(&mut process, opcode, payload);
            tokio::pin!(exchange);
            let timeout = tokio::time::sleep(HELPER_REQUEST_TIMEOUT);
            tokio::pin!(timeout);
            tokio::select! {
                biased;
                _ = wait_for_stream_cancel(cancel) => {
                    Err(anyhow::anyhow!("clipboard stream canceled"))
                }
                _ = &mut timeout => {
                    Err(anyhow::anyhow!("clipboard helper timed out"))
                }
                result = &mut exchange => result,
            }
        };

        match response {
            Ok((STATUS_OK, payload)) => {
                self.process = Some(process);
                Ok(payload)
            }
            Ok((STATUS_ERROR, payload)) => {
                self.process = Some(process);
                bail!(
                    "Android clipboard helper failed: {}",
                    String::from_utf8_lossy(&payload)
                )
            }
            Ok((other, _)) => {
                Self::stop_process(process).await;
                bail!("invalid clipboard helper status {other}")
            }
            Err(error) => {
                Self::stop_process(process).await;
                Err(error).context("persistent clipboard helper I/O failed")
            }
        }
    }

    pub(crate) async fn get(&mut self, cancel: &mut watch::Receiver<bool>) -> Result<String> {
        let response = self.request(OP_GET, &[], cancel).await?;
        String::from_utf8(response).context("Android clipboard is not valid UTF-8")
    }

    pub(crate) async fn set(
        &mut self,
        text: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        self.request(OP_SET, text.as_bytes(), cancel).await?;
        Ok(())
    }

    pub(crate) async fn clear(&mut self, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        self.request(OP_CLEAR, &[], cancel).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_CLIPBOARD_TEXT, OP_SET, STATUS_OK, exchange_io};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn helper_codec_preserves_binary_payload() {
        let (client, mut server) = tokio::io::duplex(1024);
        let (mut reader, mut writer) = tokio::io::split(client);
        let request = b"hello\nworld\0utf8:\xED\x95\x9C\xEA\xB8\x80";
        let expected = request.to_vec();
        let server_task = tokio::spawn(async move {
            let mut header = [0u8; 5];
            server.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], OP_SET);
            let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
            let mut payload = vec![0u8; length];
            server.read_exact(&mut payload).await.unwrap();
            assert_eq!(payload, expected);

            let response = b"ok\n\0";
            let mut response_header = [0u8; 5];
            response_header[0] = STATUS_OK;
            response_header[1..].copy_from_slice(&(response.len() as u32).to_be_bytes());
            server.write_all(&response_header).await.unwrap();
            server.write_all(response).await.unwrap();
        });
        let (status, payload) = exchange_io(&mut reader, &mut writer, OP_SET, request)
            .await
            .unwrap();
        assert_eq!(status, STATUS_OK);
        assert_eq!(payload, b"ok\n\0");
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn helper_codec_rejects_oversized_response() {
        let (client, mut server) = tokio::io::duplex(128);
        let (mut reader, mut writer) = tokio::io::split(client);
        let server_task = tokio::spawn(async move {
            let mut header = [0u8; 5];
            server.read_exact(&mut header).await.unwrap();
            let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
            let mut payload = vec![0u8; length];
            server.read_exact(&mut payload).await.unwrap();

            let mut response_header = [0u8; 5];
            response_header[0] = STATUS_OK;
            response_header[1..].copy_from_slice(&((MAX_CLIPBOARD_TEXT as u32) + 1).to_be_bytes());
            server.write_all(&response_header).await.unwrap();
        });
        let error = exchange_io(&mut reader, &mut writer, OP_SET, b"x")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds 4 MiB"));
        server_task.await.unwrap();
    }
}
