use axum::body::Bytes;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::{Mutex, oneshot};
use tracing::error;

/// Body that tees data into a buffer and signals completion.
///
/// This is used to capture the proxied payload so it can be processed by the interceptor.
///
/// The completion signal is sent when the body is fully read.
///
/// The buffer is used to store the proxied payload.
pub struct TeeBodyWithCompletion<B> {
    inner: B,
    buffer: Arc<Mutex<Vec<u8>>>,
    completion_sender: Arc<Mutex<Option<oneshot::Sender<Bytes>>>>,
}

impl<B> TeeBodyWithCompletion<B> {
    pub fn new(body: B) -> (Self, oneshot::Receiver<Bytes>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let (completion_sender, completion_receiver) = oneshot::channel();

        let tee_body = Self {
            inner: body,
            buffer,
            completion_sender: Arc::new(Mutex::new(Some(completion_sender))),
        };

        (tee_body, completion_receiver)
    }

    fn send_completion(&self) {
        let buffer = self.buffer.clone();
        let sender = self.completion_sender.clone();

        tokio::spawn(async move {
            let mut sender_guard = sender.lock().await;
            if let Some(sender) = sender_guard.take() {
                let collected_data = {
                    let buffer_guard = buffer.lock().await;
                    let data = buffer_guard.clone();
                    Bytes::from(data)
                };

                if sender.send(collected_data).is_err() {
                    error!(
                        "PROXY | tee_body | unable to send completion signal, proxied payload won't be processed"
                    );
                }
            }
        });
    }
}

impl<B> http_body::Body for TeeBodyWithCompletion<B>
where
    B: http_body::Body,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    B::Data: AsRef<[u8]> + Send + Sync + 'static,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        // SAFETY: This is safe because:
        // 1. We're only accessing the `inner` field, which is the only field that needs pinning
        // 2. The `inner` field implements `http_body::Body` which has proper pinning guarantees
        // 3. We're not moving or dropping the pinned data, just calling methods on it
        // 4. The `Arc<Mutex<...>>` fields don't require pinning as they're shared references
        let inner = unsafe { self.as_mut().map_unchecked_mut(|s| &mut s.inner) };

        match inner.poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let buffer = self.buffer.clone();
                    let data_bytes = data.as_ref().to_vec();

                    // Use try_lock here since we're in a sync context and don't want to block
                    // If it fails, we'll just skip this chunk of data rather than blocking
                    if let Ok(mut buf) = buffer.try_lock() {
                        buf.extend_from_slice(&data_bytes);
                    }
                }

                // After processing the frame, check if the inner body is now at end of stream
                // This handles cases where the body completes after receiving the final data frame
                if self.inner.is_end_stream() {
                    self.send_completion();
                }

                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                self.send_completion();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => {
                // Send completion even on error so receiver doesn't hang
                self.send_completion();
                Poll::Ready(Some(Err(e)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
