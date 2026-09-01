use anyhow::Result;
use sidewire_protocol::{Frame, FrameKind, SecureFrameWriter, SharedNoise};
use std::sync::Arc;
use tokio::{net::tcp::OwnedWriteHalf, sync::Mutex};

#[derive(Clone)]
pub(super) struct MuxWriter {
    inner: Arc<Mutex<SecureFrameWriter<OwnedWriteHalf>>>,
}

impl MuxWriter {
    pub(super) fn new(writer: OwnedWriteHalf, noise: Option<SharedNoise>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SecureFrameWriter::new(writer, noise))),
        }
    }

    pub(super) async fn send(&self, frame: &Frame) -> Result<()> {
        let mut writer = self.inner.lock().await;
        writer.write_frame(frame).await
    }

    pub(super) async fn send_raw(
        &self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        let mut writer = self.inner.lock().await;
        writer.write_raw(kind, stream_id, payload).await
    }
    pub(super) async fn send_error(&self, stream_id: u32, error: impl std::fmt::Display) {
        let message = error.to_string();
        let _ = self
            .send_raw(FrameKind::Error, stream_id, message.as_bytes())
            .await;
    }
}
