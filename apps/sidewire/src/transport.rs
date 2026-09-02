use anyhow::{Context, Result, bail};
use sidewire_protocol::{Frame, FrameKind, SecureFrameReader, SecureFrameWriter, SharedNoise};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tokio::{
    net::{TcpStream, tcp::OwnedWriteHalf},
    sync::{Mutex, Notify, mpsc},
};

const ROUTE_CAPACITY: usize = 16;

#[derive(Clone)]
pub(super) struct DeviceTransport {
    inner: Arc<TransportInner>,
}

struct TransportInner {
    writer: Mutex<SecureFrameWriter<OwnedWriteHalf>>,
    routes: Mutex<HashMap<u32, mpsc::Sender<Frame>>>,
    next_stream_id: AtomicU32,
    closed: AtomicBool,
    closed_notify: Notify,
}
pub(super) struct DeviceStream {
    id: u32,
    transport: DeviceTransport,
    receiver: mpsc::Receiver<Frame>,
}

impl DeviceTransport {
    pub(super) fn new(stream: TcpStream, noise: Option<SharedNoise>) -> Self {
        let (reader, writer) = stream.into_split();
        let mut reader = SecureFrameReader::new(reader, noise.clone());
        let writer = SecureFrameWriter::new(writer, noise);
        let inner = Arc::new(TransportInner {
            writer: Mutex::new(writer),
            routes: Mutex::new(HashMap::new()),
            next_stream_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            closed_notify: Notify::new(),
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
                let sender = { inner.routes.lock().await.get(&frame.stream_id).cloned() };
                match sender {
                    Some(sender) => {
                        let stream_id = frame.stream_id;
                        if sender.send(frame).await.is_err() {
                            inner.routes.lock().await.remove(&stream_id);
                        }
                    }
                    None if frame.kind == FrameKind::Pong => tracing::debug!(
                        stream_id = frame.stream_id,
                        "dropping late heartbeat response"
                    ),
                    None => tracing::warn!(
                        stream_id = frame.stream_id,
                        kind = ?frame.kind,
                        "dropping frame for unknown device stream"
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
            let mut routes = self.inner.routes.lock().await;
            if self.inner.closed.load(Ordering::Acquire) {
                bail!("device connection is closed");
            }
            if routes.contains_key(&id) {
                continue;
            }
            routes.insert(id, sender);
            return Ok(DeviceStream {
                id,
                transport: self.clone(),
                receiver,
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

    pub(super) async fn send(&self, frame: &Frame) -> Result<()> {
        if self.transport.inner.closed.load(Ordering::Acquire) {
            bail!("device connection is closed");
        }
        if frame.stream_id != self.id {
            bail!(
                "frame stream {} does not match routed stream {}",
                frame.stream_id,
                self.id
            );
        }
        let mut writer = self.transport.inner.writer.lock().await;
        writer.write_frame(frame).await
    }

    pub(super) async fn send_raw(&self, kind: FrameKind, payload: &[u8]) -> Result<()> {
        if self.transport.inner.closed.load(Ordering::Acquire) {
            bail!("device connection is closed");
        }
        let mut writer = self.transport.inner.writer.lock().await;
        writer.write_raw(kind, self.id, payload).await
    }

    pub(super) async fn recv(&mut self) -> Result<Frame> {
        self.receiver
            .recv()
            .await
            .context("device connection closed while stream was active")
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
