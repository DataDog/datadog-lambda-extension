// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;
use tracing::warn;

use crate::traces::span_dedup::{DedupKey, Deduper};

#[derive(Debug, thiserror::Error)]
pub enum DedupError {
    #[error("Failed to send command to deduper: {0}")]
    SendError(mpsc::error::SendError<DedupCommand>),
    #[error("Failed to receive response from deduper: {0}")]
    RecvError(oneshot::error::RecvError),
    #[error("Timeout waiting for response from deduper")]
    Timeout,
}

pub enum DedupCommand {
    CheckAndAdd(DedupKey, oneshot::Sender<bool>),
}

#[derive(Clone)]
pub struct DedupHandle {
    tx: mpsc::UnboundedSender<DedupCommand>,
}

impl DedupHandle {
    #[must_use]
    pub fn new(tx: mpsc::UnboundedSender<DedupCommand>) -> Self {
        Self { tx }
    }

    /// Checks if a span key exists and adds it if it doesn't.
    /// Returns `true` if the key was added (didn't exist), `false` if it already existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot be sent to the deduper service,
    /// if the response cannot be received, or if the operation times out after 5 seconds.
    pub async fn check_and_add(&self, key: DedupKey) -> Result<bool, DedupError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(DedupCommand::CheckAndAdd(key, response_tx))
            .map_err(DedupError::SendError)?;

        // Sometimes the dedup service fails to send a response for unknown reasons, so we 
        // timeout after 5 seconds to avoid blocking the caller forever. We may remove the
        // timeout if we can figure out and fix the root cause.
        tokio::time::timeout(Duration::from_secs(5), response_rx)
            .await
            .map_err(|_| DedupError::Timeout)?
            .map_err(DedupError::RecvError)
    }
}

pub struct DedupService {
    deduper: Deduper,
    rx: mpsc::UnboundedReceiver<DedupCommand>,
}

/// A service that handles `check_and_add()` requests without using mutex.
impl DedupService {
    /// Creates a new dedup service with the default capacity (100).
    #[must_use]
    pub fn new() -> (Self, DedupHandle) {
        Self::with_capacity(100)
    }

    /// Creates a new dedup service with the specified capacity.
    #[must_use]
    fn with_capacity(capacity: usize) -> (Self, DedupHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = DedupHandle::new(tx);
        let service = Self {
            deduper: Deduper::new(capacity),
            rx,
        };
        (service, handle)
    }

    /// Runs the dedup service, processing commands until the channel is closed.
    pub async fn run(mut self) {
        while let Some(command) = self.rx.recv().await {
            match command {
                DedupCommand::CheckAndAdd(key, response_tx) => {
                    let was_added = self.deduper.check_and_add(key);
                    if let Err(e) = response_tx.send(was_added) {
                        warn!("Failed to send check_and_add response: {e:?}");
                    }
                }
            }
        }
    }
}

impl Default for DedupService {
    fn default() -> Self {
        let (_tx, rx) = mpsc::unbounded_channel();
        Self {
            deduper: Deduper::default(),
            rx,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dedup_service_check_and_add() {
        let (service, handle) = DedupService::new();

        tokio::spawn(async move {
            service.run().await;
        });

        let key1 = DedupKey::new(100, 123);
        let key2 = DedupKey::new(100, 456);

        // First call should return true (key was added)
        assert!(handle.check_and_add(key1).await.unwrap());

        // Second call should return false (key already exists)
        assert!(!handle.check_and_add(key1).await.unwrap());

        // Different key should return true again
        assert!(handle.check_and_add(key2).await.unwrap());

        // Calling again on already-added keys should return false
        assert!(!handle.check_and_add(key1).await.unwrap());
        assert!(!handle.check_and_add(key2).await.unwrap());
    }

    #[tokio::test]
    async fn test_dedup_service_eviction() {
        let (service, handle) = DedupService::with_capacity(3);

        tokio::spawn(async move {
            service.run().await;
        });

        let key1 = DedupKey::new(1, 10);
        let key2 = DedupKey::new(2, 20);
        let key3 = DedupKey::new(3, 30);
        let key4 = DedupKey::new(4, 40);

        // Add 3 keys
        assert!(handle.check_and_add(key1).await.unwrap());
        assert!(handle.check_and_add(key2).await.unwrap());
        assert!(handle.check_and_add(key3).await.unwrap());

        // Add a 4th key, should evict the oldest (key1)
        assert!(handle.check_and_add(key4).await.unwrap());

        // Now key1 should be addable again (was evicted)
        assert!(handle.check_and_add(key1).await.unwrap());

        // But key2 should now be evicted
        assert!(handle.check_and_add(key2).await.unwrap());
    }
}
