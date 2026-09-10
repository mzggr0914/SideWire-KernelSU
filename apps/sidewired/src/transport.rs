use anyhow::{Result, bail};
use sidewire_protocol::{Frame, FrameKind, SecureFrameWriter, SharedNoise, raw_frame};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    net::tcp::OwnedWriteHalf,
    sync::{Notify, mpsc, oneshot},
};

const WRITER_QUEUE_CAPACITY: usize = 16;

struct WriterCommand {
    frame: Frame,
    done: oneshot::Sender<std::result::Result<(), String>>,
}

#[derive(Clone)]
pub(super) struct MuxWriter {
    inner: mpsc::Sender<WriterCommand>,
    canceled: Arc<StdMutex<HashSet<u32>>>,
    failed: Arc<AtomicBool>,
    failed_notify: Arc<Notify>,
    shutdown_notify: Arc<Notify>,
    supports_stream_cancel: bool,
}

impl MuxWriter {
    pub(super) fn new(
        writer: OwnedWriteHalf,
        noise: Option<SharedNoise>,
        supports_stream_cancel: bool,
    ) -> Self {
        let (writer_tx, mut writer_rx) = mpsc::channel(WRITER_QUEUE_CAPACITY);
        let failed = Arc::new(AtomicBool::new(false));
        let failed_notify = Arc::new(Notify::new());
        let shutdown_notify = Arc::new(Notify::new());
        let task_failed = failed.clone();
        let task_notify = failed_notify.clone();
        let task_shutdown = shutdown_notify.clone();
        tokio::spawn(async move {
            let mut writer = SecureFrameWriter::new(writer, noise);
            loop {
                let shutdown = task_shutdown.notified();
                if task_failed.load(Ordering::Acquire) {
                    break;
                }
                let command = tokio::select! {
                    _ = shutdown => break,
                    command = writer_rx.recv() => command,
                };
                let Some(WriterCommand { frame, done }) = command else {
                    break;
                };
                let shutdown = task_shutdown.notified();
                let result = if task_failed.load(Ordering::Acquire) {
                    Err(anyhow::anyhow!("device connection writer stopped"))
                } else {
                    tokio::select! {
                        _ = shutdown => Err(anyhow::anyhow!("device connection writer stopped")),
                        result = writer.write_frame(&frame) => result,
                    }
                };
                match result {
                    Ok(()) => {
                        let _ = done.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = done.send(Err(error.to_string()));
                        if !task_failed.load(Ordering::Acquire) {
                            tracing::warn!(%error, "device mux writer ended");
                        }
                        break;
                    }
                }
            }
            task_failed.store(true, Ordering::Release);
            task_notify.notify_waiters();
        });
        Self {
            inner: writer_tx,
            canceled: Arc::new(StdMutex::new(HashSet::new())),
            failed,
            failed_notify,
            shutdown_notify,
            supports_stream_cancel,
        }
    }

    async fn queue_frame(&self, frame: Frame) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            bail!("device connection writer stopped");
        }
        let (done, completed) = oneshot::channel();
        self.inner
            .send(WriterCommand { frame, done })
            .await
            .map_err(|_| anyhow::anyhow!("device connection writer stopped"))?;
        match completed.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => bail!(message),
            Err(_) => bail!("device connection writer stopped"),
        }
    }

    pub(super) fn close(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            self.shutdown_notify.notify_waiters();
            self.failed_notify.notify_waiters();
        }
    }

    pub(super) async fn wait_failed(&self) {
        loop {
            let notified = self.failed_notify.notified();
            if self.failed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn cancel_local(&self, stream_id: u32) {
        self.canceled.lock().unwrap().insert(stream_id);
    }

    pub(super) fn finish_stream(&self, stream_id: u32) {
        self.canceled.lock().unwrap().remove(&stream_id);
    }

    pub(super) fn is_canceled(&self, stream_id: u32) -> bool {
        self.canceled.lock().unwrap().contains(&stream_id)
    }

    fn ensure_active(&self, stream_id: u32) -> Result<()> {
        if self.is_canceled(stream_id) {
            bail!("stream {stream_id} canceled");
        }
        Ok(())
    }

    pub(super) async fn send(&self, frame: &Frame) -> Result<()> {
        self.ensure_active(frame.stream_id)?;
        let result = self.queue_frame(frame.clone()).await;
        self.ensure_active(frame.stream_id)?;
        result
    }

    pub(super) async fn send_raw(
        &self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        self.ensure_active(stream_id)?;
        let result = self
            .queue_frame(raw_frame(kind, stream_id, payload.to_vec()))
            .await;
        self.ensure_active(stream_id)?;
        result
    }

    pub(super) async fn send_cancel(&self, stream_id: u32, reason: impl std::fmt::Display) {
        self.cancel_local(stream_id);
        let message = reason.to_string();
        let kind = if self.supports_stream_cancel {
            FrameKind::StreamCancel
        } else {
            FrameKind::Error
        };
        if let Err(error) = self
            .queue_frame(raw_frame(kind, stream_id, message.into_bytes()))
            .await
        {
            tracing::debug!(stream_id, %error, "failed to send stream cancellation");
        }
    }

    pub(super) async fn send_error(&self, stream_id: u32, error: impl std::fmt::Display) {
        if self.is_canceled(stream_id) {
            return;
        }
        let message = error.to_string();
        let _ = self
            .send_raw(FrameKind::Error, stream_id, message.as_bytes())
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::MuxWriter;
    use sidewire_protocol::{FrameKind, SecureFrameReader};
    use tokio::net::{TcpListener, TcpStream};

    async fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        (client.unwrap(), accepted.unwrap().0)
    }

    #[tokio::test]
    async fn cancel_is_stream_scoped_and_signaled() {
        let (client, server) = tcp_pair().await;
        let (_, write_half) = client.into_split();
        let writer = MuxWriter::new(write_half, None, true);
        let mut reader = SecureFrameReader::new(server, None);

        writer.send_cancel(7, "overflow").await;
        let frame = reader.read_frame().await.unwrap();
        assert_eq!(frame.kind, FrameKind::StreamCancel);
        assert_eq!(frame.stream_id, 7);
        assert_eq!(frame.payload, b"overflow");
        assert!(
            writer
                .send_raw(FrameKind::FileChunk, 7, b"late")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn legacy_peer_gets_error_instead_of_unknown_cancel_kind() {
        let (client, server) = tcp_pair().await;
        let (_, write_half) = client.into_split();
        let writer = MuxWriter::new(write_half, None, false);
        let mut reader = SecureFrameReader::new(server, None);

        writer.send_cancel(9, "overflow").await;
        let frame = reader.read_frame().await.unwrap();
        assert_eq!(frame.kind, FrameKind::Error);
        assert_eq!(frame.stream_id, 9);
        assert_eq!(frame.payload, b"overflow");
    }
}
