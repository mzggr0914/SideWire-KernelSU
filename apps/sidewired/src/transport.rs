use anyhow::Result;
use sidewire_protocol::{Frame, FrameKind, write_frame, write_raw_frame};
use std::sync::Arc;
use tokio::{net::tcp::OwnedWriteHalf, sync::Mutex};

#[derive(Clone)]
pub(super) struct MuxWriter {
    inner: Arc<Mutex<OwnedWriteHalf>>,
}

impl MuxWriter {
    pub(super) fn new(writer: OwnedWriteHalf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(writer)),
        }
    }

    pub(super) async fn send(&self, frame: &Frame) -> Result<()> {
        let mut writer = self.inner.lock().await;
        write_frame(&mut *writer, frame).await
    }

    pub(super) async fn send_raw(
        &self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        let mut writer = self.inner.lock().await;
        write_raw_frame(&mut *writer, kind, stream_id, payload).await
    }
    pub(super) async fn send_error(&self, stream_id: u32, error: impl std::fmt::Display) {
        let message = error.to_string();
        let _ = self
            .send_raw(FrameKind::Error, stream_id, message.as_bytes())
            .await;
    }
}
