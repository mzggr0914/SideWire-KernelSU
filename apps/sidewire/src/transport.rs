use anyhow::{Context, Result, bail};
use sidewire_protocol::{
    Frame, FrameKind, SecureFrameReader, SecureFrameWriter, SharedNoise, StreamWindowUpdate,
    capabilities, decode, frame, protocol_minor, raw_frame,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tokio::{
    net::TcpStream,
    sync::{Notify, mpsc, oneshot, watch},
};

const ROUTE_CAPACITY: usize = 16;
const WRITER_QUEUE_CAPACITY: usize = 16;
const STREAM_CANCEL_PROTOCOL_MINOR: u8 = 1;
const FLOW_CONTROL_PROTOCOL_MINOR: u8 = 2;
const MAX_FLOW_CREDIT: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct DeviceTransport {
    inner: Arc<TransportInner>,
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
        let updated = available
            .checked_add(bytes)
            .context("stream flow credit overflow")?;
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
struct RouteSender {
    sender: mpsc::Sender<Frame>,
    cancel: watch::Sender<Option<String>>,
    flow: Option<Arc<FlowCredit>>,
}

struct WriterCommand {
    frame: Frame,
    done: oneshot::Sender<std::result::Result<(), String>>,
}

struct TransportInner {
    writer: mpsc::Sender<WriterCommand>,
    routes: StdMutex<HashMap<u32, RouteSender>>,
    next_stream_id: AtomicU32,
    closed: AtomicBool,
    shutdown_notify: Notify,
    closed_notify: Notify,
    supports_stream_cancel: bool,
    supports_flow_control: bool,
}

pub(super) struct DeviceStream {
    id: u32,
    transport: DeviceTransport,
    receiver: mpsc::Receiver<Frame>,
    cancel: watch::Receiver<Option<String>>,
    flow: Option<Arc<FlowCredit>>,
    completed: bool,
}

fn frame_completes_stream(kind: FrameKind) -> bool {
    matches!(
        kind,
        FrameKind::ExecExit
            | FrameKind::FileEnd
            | FrameKind::PtyExit
            | FrameKind::ClipboardData
            | FrameKind::Pong
            | FrameKind::ProxyStartAck
            | FrameKind::Error
    )
}

fn signal_closed(inner: &TransportInner) {
    if !inner.closed.swap(true, Ordering::AcqRel) {
        inner.shutdown_notify.notify_waiters();
        inner.closed_notify.notify_waiters();
    }
}

async fn enqueue_write(
    inner: &Arc<TransportInner>,
    frame: Frame,
) -> Result<oneshot::Receiver<std::result::Result<(), String>>> {
    if inner.closed.load(Ordering::Acquire) {
        bail!("device connection is closed");
    }
    let (done, completed) = oneshot::channel();
    inner
        .writer
        .send(WriterCommand { frame, done })
        .await
        .context("device connection writer stopped")?;
    Ok(completed)
}

async fn queued_write(inner: &Arc<TransportInner>, frame: Frame) -> Result<()> {
    match enqueue_write(inner, frame).await?.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => bail!(message),
        Err(_) => bail!("device connection writer stopped"),
    }
}

async fn send_stream_cancel(inner: Arc<TransportInner>, stream_id: u32, reason: String) {
    if !inner.supports_stream_cancel || inner.closed.load(Ordering::Acquire) {
        return;
    }
    if let Err(error) = queued_write(
        &inner,
        raw_frame(FrameKind::StreamCancel, stream_id, reason.into_bytes()),
    )
    .await
    {
        tracing::debug!(stream_id, %error, "failed to send stream cancellation");
    }
}

impl DeviceTransport {
    pub(super) fn new(
        stream: TcpStream,
        noise: Option<SharedNoise>,
        protocol: u16,
        peer_capabilities: u64,
    ) -> Self {
        let (reader, writer_half) = stream.into_split();
        let mut reader = SecureFrameReader::new(reader, noise.clone());
        let (writer_tx, mut writer_rx) = mpsc::channel(WRITER_QUEUE_CAPACITY);
        let inner = Arc::new(TransportInner {
            writer: writer_tx,
            routes: StdMutex::new(HashMap::new()),
            next_stream_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            shutdown_notify: Notify::new(),
            closed_notify: Notify::new(),
            supports_stream_cancel: protocol_minor(protocol) >= STREAM_CANCEL_PROTOCOL_MINOR
                && peer_capabilities & capabilities::STREAM_CANCEL != 0,
            supports_flow_control: protocol_minor(protocol) >= FLOW_CONTROL_PROTOCOL_MINOR
                && peer_capabilities & capabilities::FLOW_CONTROL != 0,
        });
        let transport = Self {
            inner: inner.clone(),
        };

        let writer_inner = inner.clone();
        tokio::spawn(async move {
            let mut writer = SecureFrameWriter::new(writer_half, noise);
            loop {
                let shutdown = writer_inner.shutdown_notify.notified();
                if writer_inner.closed.load(Ordering::Acquire) {
                    break;
                }
                let command = tokio::select! {
                    _ = shutdown => break,
                    command = writer_rx.recv() => command,
                };
                let Some(WriterCommand { frame, done }) = command else {
                    break;
                };
                let shutdown = writer_inner.shutdown_notify.notified();
                let result = if writer_inner.closed.load(Ordering::Acquire) {
                    Err(anyhow::anyhow!("device connection is closed"))
                } else {
                    tokio::select! {
                        _ = shutdown => Err(anyhow::anyhow!("device connection is closed")),
                        result = writer.write_frame(&frame) => result,
                    }
                };
                match result {
                    Ok(()) => {
                        let _ = done.send(Ok(()));
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = done.send(Err(message));
                        tracing::warn!(%error, "device transport writer ended");
                        signal_closed(&writer_inner);
                        break;
                    }
                }
            }
            signal_closed(&writer_inner);
        });

        tokio::spawn(async move {
            loop {
                let shutdown = inner.shutdown_notify.notified();
                if inner.closed.load(Ordering::Acquire) {
                    break;
                }
                let incoming = tokio::select! {
                    _ = shutdown => break,
                    incoming = reader.read_frame() => incoming,
                };
                let frame = match incoming {
                    Ok(frame) => frame,
                    Err(error) => {
                        tracing::warn!(%error, "device transport reader ended");
                        break;
                    }
                };
                let stream_id = frame.stream_id;
                if frame.kind == FrameKind::StreamWindowUpdate {
                    if !inner.supports_flow_control {
                        tracing::warn!(stream_id, "unexpected stream window update from peer");
                        break;
                    }
                    let update = match decode::<StreamWindowUpdate>(&frame.payload) {
                        Ok(update) => update,
                        Err(error) => {
                            tracing::warn!(stream_id, %error, "invalid stream window update");
                            break;
                        }
                    };
                    let flow = inner
                        .routes
                        .lock()
                        .unwrap()
                        .get(&stream_id)
                        .and_then(|route| route.flow.clone());
                    if let Some(flow) = flow {
                        if let Err(error) = flow.add(update.bytes as usize) {
                            tracing::warn!(stream_id, %error, "invalid stream flow credit");
                            break;
                        }
                    } else {
                        tracing::debug!(stream_id, "dropping window update for inactive stream");
                    }
                    continue;
                }
                if frame.kind == FrameKind::StreamCancel {
                    let route = inner.routes.lock().unwrap().remove(&stream_id);
                    if let Some(route) = route {
                        let reason = if frame.payload.is_empty() {
                            format!("remote canceled stream {stream_id}")
                        } else {
                            String::from_utf8_lossy(&frame.payload).into_owned()
                        };
                        let _ = route.cancel.send(Some(reason));
                    } else {
                        tracing::debug!(stream_id, "dropping cancellation for inactive stream");
                    }
                    continue;
                }

                let route = { inner.routes.lock().unwrap().get(&stream_id).cloned() };
                match route {
                    Some(route) => match route.sender.try_send(frame) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            inner.routes.lock().unwrap().remove(&stream_id);
                            let reason = format!(
                                "stream {stream_id} receive queue overflow; stream canceled"
                            );
                            let _ = route.cancel.send(Some(reason.clone()));
                            tracing::warn!(stream_id, "device stream receive queue overflow");
                            tokio::spawn(send_stream_cancel(inner.clone(), stream_id, reason));
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            inner.routes.lock().unwrap().remove(&stream_id);
                        }
                    },
                    None if frame.kind == FrameKind::Pong => {
                        tracing::debug!(stream_id, "dropping late heartbeat response")
                    }
                    None => tracing::debug!(
                        stream_id,
                        kind = ?frame.kind,
                        "dropping frame for inactive device stream"
                    ),
                }
            }
            signal_closed(&inner);
            inner.routes.lock().unwrap().clear();
        });

        transport
    }

    pub(super) fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(super) fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    pub(super) async fn close(&self) {
        signal_closed(&self.inner);
        self.inner.routes.lock().unwrap().clear();
    }

    pub(super) async fn send_frame(&self, frame: &Frame) -> Result<()> {
        queued_write(&self.inner, frame.clone()).await
    }

    pub(super) async fn send_raw(
        &self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        queued_write(&self.inner, raw_frame(kind, stream_id, payload.to_vec())).await
    }

    pub(super) async fn open_stream(&self) -> Result<DeviceStream> {
        if self.inner.closed.load(Ordering::Acquire) {
            bail!("device connection is closed");
        }
        loop {
            let id = self.inner.next_stream_id.fetch_add(1, Ordering::Relaxed);
            if id == 0 {
                continue;
            }
            let (sender, receiver) = mpsc::channel(ROUTE_CAPACITY);
            let (cancel_sender, cancel) = watch::channel(None);
            let flow = self
                .inner
                .supports_flow_control
                .then(|| Arc::new(FlowCredit::new()));
            let mut routes = self.inner.routes.lock().unwrap();
            if self.inner.closed.load(Ordering::Acquire) {
                bail!("device connection is closed");
            }
            if routes.contains_key(&id) {
                continue;
            }
            routes.insert(
                id,
                RouteSender {
                    sender,
                    cancel: cancel_sender,
                    flow: flow.clone(),
                },
            );
            return Ok(DeviceStream {
                id,
                transport: self.clone(),
                receiver,
                cancel,
                flow,
                completed: false,
            });
        }
    }

    pub(super) async fn wait_closed(&self) {
        loop {
            let notified = self.inner.closed_notify.notified();
            if self.inner.closed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

impl DeviceStream {
    pub(super) fn id(&self) -> u32 {
        self.id
    }

    fn cancel_reason(&self) -> Option<String> {
        self.cancel.borrow().clone()
    }

    fn ensure_active(&self) -> Result<()> {
        if let Some(reason) = self.cancel_reason() {
            bail!(reason);
        }
        if self.transport.inner.closed.load(Ordering::Acquire) {
            bail!("device connection is closed");
        }
        Ok(())
    }

    async fn acquire_flow_credit(&self, bytes: usize) -> Result<()> {
        let Some(flow) = &self.flow else {
            return Ok(());
        };
        if bytes == 0 {
            return Ok(());
        }
        if bytes > MAX_FLOW_CREDIT {
            bail!("frame requires more than {MAX_FLOW_CREDIT} bytes of flow credit");
        }
        let mut cancel = self.cancel.clone();
        loop {
            let credit_ready = flow.notify.notified();
            let closed = self.transport.inner.closed_notify.notified();
            self.ensure_active()?;
            if flow.try_take(bytes) {
                return Ok(());
            }
            tokio::select! {
                _ = credit_ready => {}
                _ = closed => self.ensure_active()?,
                changed = cancel.changed() => {
                    if changed.is_ok()
                        && let Some(reason) = cancel.borrow().clone()
                    {
                        bail!(reason);
                    }
                    self.ensure_active()?;
                }
            }
        }
    }

    pub(super) fn uses_flow_control(&self) -> bool {
        self.flow.is_some()
    }

    pub(super) async fn grant_window(&self, bytes: usize) -> Result<()> {
        if !self.uses_flow_control() || bytes == 0 {
            return Ok(());
        }
        let bytes = u32::try_from(bytes).context("stream window update exceeds u32")?;
        self.send(&frame(
            FrameKind::StreamWindowUpdate,
            self.id,
            &StreamWindowUpdate { bytes },
        )?)
        .await
    }

    pub(super) async fn send(&self, frame: &Frame) -> Result<()> {
        self.ensure_active()?;
        if frame.stream_id != self.id {
            bail!(
                "frame stream {} does not match routed stream {}",
                frame.stream_id,
                self.id
            );
        }
        if frame.kind == FrameKind::FileChunk {
            self.acquire_flow_credit(frame.payload.len()).await?;
            self.ensure_active()?;
        }
        let result = self.transport.send_frame(frame).await;
        self.ensure_active()?;
        result
    }

    pub(super) async fn send_raw(&self, kind: FrameKind, payload: &[u8]) -> Result<()> {
        self.ensure_active()?;
        if kind == FrameKind::FileChunk {
            self.acquire_flow_credit(payload.len()).await?;
            self.ensure_active()?;
        }
        let result = self.transport.send_raw(kind, self.id, payload).await;
        self.ensure_active()?;
        result
    }

    pub(super) fn try_recv(&mut self) -> Result<Option<Frame>> {
        if let Some(reason) = self.cancel_reason() {
            self.completed = true;
            bail!(reason);
        }
        match self.receiver.try_recv() {
            Ok(frame) => {
                self.completed |= frame_completes_stream(frame.kind);
                Ok(Some(frame))
            }
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                if let Some(reason) = self.cancel_reason() {
                    self.completed = true;
                    bail!(reason);
                }
                bail!("device connection closed while stream was active")
            }
        }
    }

    pub(super) async fn recv(&mut self) -> Result<Frame> {
        if let Some(reason) = self.cancel_reason() {
            self.completed = true;
            bail!(reason);
        }
        let frame = tokio::select! {
            biased;
            changed = self.cancel.changed() => {
                if changed.is_ok()
                    && let Some(reason) = self.cancel_reason()
                {
                    self.completed = true;
                    bail!(reason);
                }
                self.receiver.recv().await
            }
            frame = self.receiver.recv() => frame,
        }
        .context("device connection closed while stream was active")?;
        self.completed |= frame_completes_stream(frame.kind);
        Ok(frame)
    }
}

impl Drop for DeviceStream {
    fn drop(&mut self) {
        let id = self.id;
        self.transport.inner.routes.lock().unwrap().remove(&id);
        if !self.completed
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let inner = self.transport.inner.clone();
            handle.spawn(async move {
                send_stream_cancel(
                    inner,
                    id,
                    "local stream dropped before completion".to_owned(),
                )
                .await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceTransport, ROUTE_CAPACITY, enqueue_write};
    use sidewire_protocol::{
        FrameKind, SecureFrameReader, SecureFrameWriter, StreamWindowUpdate, VERSION, capabilities,
        frame, raw_frame,
    };
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
    async fn dropping_write_waiter_does_not_truncate_frame() {
        let (client, server) = tcp_pair().await;
        let transport = DeviceTransport::new(client, None, VERSION, capabilities::STREAM_CANCEL);
        let payload = vec![0x5au8; 4 * 1024 * 1024];
        let completed = enqueue_write(
            &transport.inner,
            raw_frame(FrameKind::FileChunk, 77, payload.clone()),
        )
        .await
        .unwrap();
        drop(completed);

        let mut reader = SecureFrameReader::new(server, None);
        let first = timeout(Duration::from_secs(5), reader.read_frame())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.kind, FrameKind::FileChunk);
        assert_eq!(first.stream_id, 77);
        assert_eq!(first.payload, payload);
        transport
            .send_raw(FrameKind::Ping, 78, b"next")
            .await
            .unwrap();
        let second = reader.read_frame().await.unwrap();
        assert_eq!(second.kind, FrameKind::Ping);
        assert_eq!(second.payload, b"next");
    }

    #[tokio::test]
    async fn dropping_active_stream_notifies_remote() {
        let (client, server) = tcp_pair().await;
        let transport = DeviceTransport::new(client, None, VERSION, capabilities::STREAM_CANCEL);
        let stream = transport.open_stream().await.unwrap();
        let id = stream.id();
        drop(stream);

        let mut reader = SecureFrameReader::new(server, None);
        let frame = timeout(Duration::from_secs(1), reader.read_frame())
            .await
            .expect("stream cancellation was not sent")
            .unwrap();
        assert_eq!(frame.kind, FrameKind::StreamCancel);
        assert_eq!(frame.stream_id, id);
    }

    #[tokio::test]
    async fn flow_control_blocks_file_chunks_until_credit() {
        let (client, server) = tcp_pair().await;
        let transport = DeviceTransport::new(
            client,
            None,
            VERSION,
            capabilities::STREAM_CANCEL | capabilities::FLOW_CONTROL,
        );
        let stream = transport.open_stream().await.unwrap();
        let stream_id = stream.id();
        let (server_read, server_write) = server.into_split();
        let mut reader = SecureFrameReader::new(server_read, None);
        let mut writer = SecureFrameWriter::new(server_write, None);

        let send = tokio::spawn(async move {
            stream
                .send_raw(FrameKind::FileChunk, b"data")
                .await
                .unwrap();
        });
        assert!(
            timeout(Duration::from_millis(50), reader.read_frame())
                .await
                .is_err()
        );
        writer
            .write_frame(
                &frame(
                    FrameKind::StreamWindowUpdate,
                    stream_id,
                    &StreamWindowUpdate { bytes: 3 },
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_millis(50), reader.read_frame())
                .await
                .is_err()
        );
        writer
            .write_frame(
                &frame(
                    FrameKind::StreamWindowUpdate,
                    stream_id,
                    &StreamWindowUpdate { bytes: 1 },
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let chunk = timeout(Duration::from_secs(1), reader.read_frame())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chunk.kind, FrameKind::FileChunk);
        assert_eq!(chunk.payload, b"data");
        send.await.unwrap();
    }

    #[tokio::test]
    async fn flow_control_wait_is_canceled_by_remote() {
        let (client, server) = tcp_pair().await;
        let transport = DeviceTransport::new(
            client,
            None,
            VERSION,
            capabilities::STREAM_CANCEL | capabilities::FLOW_CONTROL,
        );
        let stream = transport.open_stream().await.unwrap();
        let stream_id = stream.id();
        let (server_read, server_write) = server.into_split();
        let mut reader = SecureFrameReader::new(server_read, None);
        let mut writer = SecureFrameWriter::new(server_write, None);
        let sending =
            tokio::spawn(async move { stream.send_raw(FrameKind::FileChunk, b"blocked").await });
        assert!(
            timeout(Duration::from_millis(50), reader.read_frame())
                .await
                .is_err()
        );
        writer
            .write_raw(FrameKind::StreamCancel, stream_id, b"stop")
            .await
            .unwrap();
        let error = timeout(Duration::from_secs(1), sending)
            .await
            .expect("flow-control waiter did not wake after cancellation")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("stop"));
    }

    #[tokio::test]
    async fn protocol_1_1_file_chunks_do_not_wait_for_flow_credit() {
        let (client, server) = tcp_pair().await;
        let transport = DeviceTransport::new(
            client,
            None,
            0x0101,
            capabilities::STREAM_CANCEL | capabilities::FLOW_CONTROL,
        );
        let stream = transport.open_stream().await.unwrap();
        assert!(!stream.uses_flow_control());
        stream
            .send_raw(FrameKind::FileChunk, b"legacy")
            .await
            .unwrap();
        let mut reader = SecureFrameReader::new(server, None);
        let chunk = timeout(Duration::from_secs(1), reader.read_frame())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chunk.kind, FrameKind::FileChunk);
        assert_eq!(chunk.payload, b"legacy");
    }

    #[tokio::test]
    async fn full_stream_does_not_block_other_streams() {
        let (client, server) = tcp_pair().await;
        let transport = DeviceTransport::new(client, None, VERSION, capabilities::STREAM_CANCEL);
        let mut blocked = transport.open_stream().await.unwrap();
        let mut healthy = transport.open_stream().await.unwrap();
        let mut writer = SecureFrameWriter::new(server, None);

        for _ in 0..=ROUTE_CAPACITY {
            writer
                .write_raw(FrameKind::PtyOutput, blocked.id(), b"blocked")
                .await
                .unwrap();
        }
        writer
            .write_raw(FrameKind::PtyOutput, healthy.id(), b"healthy")
            .await
            .unwrap();

        let frame = timeout(Duration::from_secs(1), healthy.recv())
            .await
            .expect("healthy stream stalled behind blocked stream")
            .unwrap();
        assert_eq!(frame.payload, b"healthy");

        let error = blocked.recv().await.unwrap_err();
        assert!(error.to_string().contains("receive queue overflow"));
        assert!(!transport.is_closed());
    }
}
