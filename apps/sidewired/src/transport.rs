use anyhow::{Result, bail};
use sidewire_protocol::{
    Frame, FrameKind, SecureFrameWriter, SharedNoise, StreamWindowUpdate, frame, raw_frame,
};
use std::{
    collections::{HashMap, HashSet},
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
const MAX_FLOW_CREDIT: usize = 16 * 1024 * 1024;

struct WriterCommand {
    frame: Frame,
    done: oneshot::Sender<std::result::Result<(), String>>,
}

struct FlowCredit {
    available: StdMutex<usize>,
    notify: Notify,
}

impl FlowCredit {
    fn new() -> Self {
        Self {
            available: StdMutex::new(0),
            notify: Notify::new(),
        }
    }

    fn add(&self, bytes: usize) -> Result<()> {
        if bytes == 0 {
            bail!("stream window update must grant at least one byte");
        }
        let mut available = self.available.lock().unwrap();
        let Some(updated) = available.checked_add(bytes) else {
            bail!("stream flow credit overflow");
        };
        if updated > MAX_FLOW_CREDIT {
            bail!("stream flow credit exceeds {MAX_FLOW_CREDIT} bytes");
        }
        *available = updated;
        drop(available);
        self.notify.notify_waiters();
        Ok(())
    }

    fn try_take(&self, bytes: usize) -> bool {
        let mut available = self.available.lock().unwrap();
        if *available < bytes {
            return false;
        }
        *available -= bytes;
        true
    }
}

#[derive(Clone)]
pub(super) struct MuxWriter {
    inner: mpsc::Sender<WriterCommand>,
    canceled: Arc<StdMutex<HashSet<u32>>>,
    failed: Arc<AtomicBool>,
    failed_notify: Arc<Notify>,
    shutdown_notify: Arc<Notify>,
    flow_credits: Arc<StdMutex<HashMap<u32, Arc<FlowCredit>>>>,
    supports_stream_cancel: bool,
    supports_flow_control: bool,
}

impl MuxWriter {
    pub(super) fn new(
        writer: OwnedWriteHalf,
        noise: Option<SharedNoise>,
        supports_stream_cancel: bool,
        supports_flow_control: bool,
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
            flow_credits: Arc::new(StdMutex::new(HashMap::new())),
            supports_stream_cancel,
            supports_flow_control,
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

    pub(super) fn register_flow_stream(&self, stream_id: u32) -> Result<()> {
        if !self.supports_flow_control {
            return Ok(());
        }
        let mut credits = self.flow_credits.lock().unwrap();
        if credits
            .insert(stream_id, Arc::new(FlowCredit::new()))
            .is_some()
        {
            bail!("stream {stream_id} already has flow control state");
        }
        Ok(())
    }

    pub(super) fn add_flow_credit(&self, stream_id: u32, bytes: u32) -> Result<bool> {
        if !self.supports_flow_control {
            bail!("peer sent flow control update without negotiation");
        }
        let flow = self.flow_credits.lock().unwrap().get(&stream_id).cloned();
        let Some(flow) = flow else {
            return Ok(false);
        };
        flow.add(bytes as usize)?;
        Ok(true)
    }

    async fn acquire_flow_credit(&self, stream_id: u32, bytes: usize) -> Result<()> {
        if !self.supports_flow_control || bytes == 0 {
            return Ok(());
        }
        if bytes > MAX_FLOW_CREDIT {
            bail!("frame requires more than {MAX_FLOW_CREDIT} bytes of flow credit");
        }
        let flow = self
            .flow_credits
            .lock()
            .unwrap()
            .get(&stream_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing flow control state for stream {stream_id}"))?;
        loop {
            let credit_ready = flow.notify.notified();
            let failed = self.failed_notify.notified();
            self.ensure_active(stream_id)?;
            if self.failed.load(Ordering::Acquire) {
                bail!("device connection writer stopped");
            }
            if flow.try_take(bytes) {
                return Ok(());
            }
            tokio::select! {
                _ = credit_ready => {}
                _ = failed => {}
            }
        }
    }

    pub(super) async fn send_window_update(&self, stream_id: u32, bytes: usize) -> Result<()> {
        if !self.supports_flow_control || bytes == 0 {
            return Ok(());
        }
        self.ensure_active(stream_id)?;
        let bytes = u32::try_from(bytes)
            .map_err(|_| anyhow::anyhow!("stream window update exceeds u32"))?;
        self.queue_frame(frame(
            FrameKind::StreamWindowUpdate,
            stream_id,
            &StreamWindowUpdate { bytes },
        )?)
        .await
    }

    pub(super) fn cancel_local(&self, stream_id: u32) {
        self.canceled.lock().unwrap().insert(stream_id);
        if let Some(flow) = self.flow_credits.lock().unwrap().get(&stream_id).cloned() {
            flow.notify.notify_waiters();
        }
    }

    pub(super) fn finish_stream(&self, stream_id: u32) {
        self.canceled.lock().unwrap().remove(&stream_id);
        self.flow_credits.lock().unwrap().remove(&stream_id);
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
        if frame.kind == FrameKind::FileChunk {
            self.acquire_flow_credit(frame.stream_id, frame.payload.len())
                .await?;
            self.ensure_active(frame.stream_id)?;
        }
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
        if kind == FrameKind::FileChunk {
            self.acquire_flow_credit(stream_id, payload.len()).await?;
            self.ensure_active(stream_id)?;
        }
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
    use tokio::{
        net::{TcpListener, TcpStream},
        time::{Duration, timeout},
    };

    async fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        (client.unwrap(), accepted.unwrap().0)
    }

    #[tokio::test]
    async fn flow_control_blocks_file_chunks_until_credit() {
        let (client, server) = tcp_pair().await;
        let (_, write_half) = client.into_split();
        let writer = MuxWriter::new(write_half, None, true, true);
        writer.register_flow_stream(11).unwrap();
        let sending = {
            let writer = writer.clone();
            tokio::spawn(async move {
                writer
                    .send_raw(FrameKind::FileChunk, 11, b"data")
                    .await
                    .unwrap();
            })
        };
        let mut reader = SecureFrameReader::new(server, None);
        assert!(
            timeout(Duration::from_millis(50), reader.read_frame())
                .await
                .is_err()
        );
        assert!(writer.add_flow_credit(11, 3).unwrap());
        assert!(
            timeout(Duration::from_millis(50), reader.read_frame())
                .await
                .is_err()
        );
        assert!(writer.add_flow_credit(11, 1).unwrap());
        let chunk = timeout(Duration::from_secs(1), reader.read_frame())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chunk.kind, FrameKind::FileChunk);
        assert_eq!(chunk.payload, b"data");
        sending.await.unwrap();
        writer.finish_stream(11);
    }

    #[tokio::test]
    async fn flow_control_wait_is_canceled_locally() {
        let (client, _server) = tcp_pair().await;
        let (_, write_half) = client.into_split();
        let writer = MuxWriter::new(write_half, None, true, true);
        writer.register_flow_stream(12).unwrap();
        let sending = {
            let writer = writer.clone();
            tokio::spawn(async move { writer.send_raw(FrameKind::FileChunk, 12, b"blocked").await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        writer.cancel_local(12);
        let error = timeout(Duration::from_secs(1), sending)
            .await
            .expect("flow-control waiter did not wake after cancellation")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("canceled"));
        writer.finish_stream(12);
    }

    #[tokio::test]
    async fn cancel_is_stream_scoped_and_signaled() {
        let (client, server) = tcp_pair().await;
        let (_, write_half) = client.into_split();
        let writer = MuxWriter::new(write_half, None, true, false);
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
        let writer = MuxWriter::new(write_half, None, false, false);
        let mut reader = SecureFrameReader::new(server, None);

        writer.send_cancel(9, "overflow").await;
        let frame = reader.read_frame().await.unwrap();
        assert_eq!(frame.kind, FrameKind::Error);
        assert_eq!(frame.stream_id, 9);
        assert_eq!(frame.payload, b"overflow");
    }
}
