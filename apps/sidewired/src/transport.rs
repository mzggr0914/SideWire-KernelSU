use anyhow::{Result, bail};
use sidewire_protocol::{Frame, FrameKind, SecureFrameWriter, SharedNoise};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::{net::tcp::OwnedWriteHalf, sync::Mutex};

#[derive(Clone)]
pub(super) struct MuxWriter {
    inner: Arc<Mutex<SecureFrameWriter<OwnedWriteHalf>>>,
    canceled: Arc<StdMutex<HashSet<u32>>>,
    supports_stream_cancel: bool,
}

impl MuxWriter {
    pub(super) fn new(
        writer: OwnedWriteHalf,
        noise: Option<SharedNoise>,
        supports_stream_cancel: bool,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SecureFrameWriter::new(writer, noise))),
            canceled: Arc::new(StdMutex::new(HashSet::new())),
            supports_stream_cancel,
        }
    }

    pub(super) fn cancel_local(&self, stream_id: u32) {
        self.canceled.lock().unwrap().insert(stream_id);
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
        let mut writer = self.inner.lock().await;
        self.ensure_active(frame.stream_id)?;
        writer.write_frame(frame).await
    }

    pub(super) async fn send_raw(
        &self,
        kind: FrameKind,
        stream_id: u32,
        payload: &[u8],
    ) -> Result<()> {
        self.ensure_active(stream_id)?;
        let mut writer = self.inner.lock().await;
        self.ensure_active(stream_id)?;
        writer.write_raw(kind, stream_id, payload).await
    }

    pub(super) async fn send_cancel(&self, stream_id: u32, reason: impl std::fmt::Display) {
        self.cancel_local(stream_id);
        let message = reason.to_string();
        let kind = if self.supports_stream_cancel {
            FrameKind::StreamCancel
        } else {
            FrameKind::Error
        };
        let mut writer = self.inner.lock().await;
        if let Err(error) = writer.write_raw(kind, stream_id, message.as_bytes()).await {
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
