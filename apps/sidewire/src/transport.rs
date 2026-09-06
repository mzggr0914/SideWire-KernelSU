use anyhow::{Context, Result, bail};
use sidewire_protocol::{
    Frame, FrameKind, SecureFrameReader, SecureFrameWriter, SharedNoise, capabilities,
    protocol_minor,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tokio::{
    net::{TcpStream, tcp::OwnedWriteHalf},
    sync::{Mutex, Notify, mpsc, watch},
};

const ROUTE_CAPACITY: usize = 16;
const STREAM_CANCEL_PROTOCOL_MINOR: u8 = 1;

#[derive(Clone)]
pub(super) struct DeviceTransport {
    inner: Arc<TransportInner>,
}

#[derive(Clone)]
struct RouteSender {
    sender: mpsc::Sender<Frame>,
    cancel: watch::Sender<Option<String>>,
}

struct TransportInner {
    writer: Mutex<SecureFrameWriter<OwnedWriteHalf>>,
    routes: Mutex<HashMap<u32, RouteSender>>,
    next_stream_id: AtomicU32,
    closed: AtomicBool,
    closed_notify: Notify,
    supports_stream_cancel: bool,
}

pub(super) struct DeviceStream {
    id: u32,
    transport: DeviceTransport,
    receiver: mpsc::Receiver<Frame>,
    cancel: watch::Receiver<Option<String>>,
}

async fn send_stream_cancel(inner: Arc<TransportInner>, stream_id: u32, reason: String) {
    if !inner.supports_stream_cancel || inner.closed.load(Ordering::Acquire) {
        return;
    }
    let mut writer = inner.writer.lock().await;
    if let Err(error) = writer
        .write_raw(FrameKind::StreamCancel, stream_id, reason.as_bytes())
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
        let (reader, writer) = stream.into_split();
        let mut reader = SecureFrameReader::new(reader, noise.clone());
        let writer = SecureFrameWriter::new(writer, noise);
        let inner = Arc::new(TransportInner {
            writer: Mutex::new(writer),
            routes: Mutex::new(HashMap::new()),
            next_stream_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            closed_notify: Notify::new(),
            supports_stream_cancel: protocol_minor(protocol) >= STREAM_CANCEL_PROTOCOL_MINOR
                && peer_capabilities & capabilities::STREAM_CANCEL != 0,
        });
        let transport = Self {
            inner: inner.clone(),
        };

        tokio::spawn(async move {
            loop {
                let frame = match reader.read_frame().await {
                    Ok(frame) => frame,
                    Err(error) => {
                        tracing::warn!(%error, "device transport reader ended");
                        break;
                    }
                };
                let stream_id = frame.stream_id;
                if frame.kind == FrameKind::StreamCancel {
                    let route = inner.routes.lock().await.remove(&stream_id);
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

                let route = { inner.routes.lock().await.get(&stream_id).cloned() };
                match route {
                    Some(route) => match route.sender.try_send(frame) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            inner.routes.lock().await.remove(&stream_id);
                            let reason = format!(
                                "stream {stream_id} receive queue overflow; stream canceled"
                            );
                            let _ = route.cancel.send(Some(reason.clone()));
                            tracing::warn!(stream_id, "device stream receive queue overflow");
                            tokio::spawn(send_stream_cancel(inner.clone(), stream_id, reason));
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            inner.routes.lock().await.remove(&stream_id);
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
            inner.closed.store(true, Ordering::Release);
            inner.routes.lock().await.clear();
            inner.closed_notify.notify_waiters();
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
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            let mut writer = self.inner.writer.lock().await;
            let _ = writer.shutdown().await;
            self.inner.closed_notify.notify_waiters();
        }
    }

    pub(super) async fn send_frame(&self, frame: &Frame) -> Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            bail!("device connection is closed");
        }
        let mut writer = self.inner.writer.lock().await;
        writer.write_frame(frame).await
    }

    pub(super) async fn send_raw(
        &self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            bail!("device connection is closed");
        }
        let mut writer = self.inner.writer.lock().await;
        writer.write_raw(kind, stream_id, payload).await
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
            let mut routes = self.inner.routes.lock().await;
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
                },
            );
            return Ok(DeviceStream {
                id,
                transport: self.clone(),
                receiver,
                cancel,
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

    pub(super) async fn send(&self, frame: &Frame) -> Result<()> {
        self.ensure_active()?;
        if frame.stream_id != self.id {
            bail!(
                "frame stream {} does not match routed stream {}",
                frame.stream_id,
                self.id
            );
        }
        let mut writer = self.transport.inner.writer.lock().await;
        self.ensure_active()?;
        writer.write_frame(frame).await
    }

    pub(super) async fn send_raw(&self, kind: FrameKind, payload: &[u8]) -> Result<()> {
        self.ensure_active()?;
        let mut writer = self.transport.inner.writer.lock().await;
        self.ensure_active()?;
        writer.write_raw(kind, self.id, payload).await
    }

    pub(super) fn try_recv(&mut self) -> Result<Option<Frame>> {
        if let Some(reason) = self.cancel_reason() {
            bail!(reason);
        }
        match self.receiver.try_recv() {
            Ok(frame) => Ok(Some(frame)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                if let Some(reason) = self.cancel_reason() {
                    bail!(reason);
                }
                bail!("device connection closed while stream was active")
            }
        }
    }

    pub(super) async fn recv(&mut self) -> Result<Frame> {
        if let Some(reason) = self.cancel_reason() {
            bail!(reason);
        }
        tokio::select! {
            biased;
            changed = self.cancel.changed() => {
                if changed.is_ok()
                    && let Some(reason) = self.cancel_reason()
                {
                    bail!(reason);
                }
                self.receiver
                    .recv()
                    .await
                    .context("device connection closed while stream was active")
            }
            frame = self.receiver.recv() => {
                frame.context("device connection closed while stream was active")
            }
        }
    }
}

impl Drop for DeviceStream {
    fn drop(&mut self) {
        let transport = self.transport.clone();
        let id = self.id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                transport.inner.routes.lock().await.remove(&id);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceTransport, ROUTE_CAPACITY};
    use sidewire_protocol::{FrameKind, SecureFrameWriter, VERSION, capabilities};
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
