// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use tokio::sync::{mpsc, oneshot};
use tracing::error;

use crate::traces::dedup::Deduper;

#[derive(Debug, thiserror::Error)]
pub enum DedupError {
    #[error("Failed to send command to deduper: {0}")]
    SendError(mpsc::error::SendError<DedupCommand>),
    #[error("Failed to receive response from deduper: {0}")]
    RecvError(oneshot::error::RecvError),
}

pub enum DedupCommand {
    CheckAndAdd(u64, oneshot::Sender<bool>),
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

    /// Checks if an ID exists and adds it if it doesn't.
    /// Returns `true` if the ID was added (didn't exist), `false` if it already existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot be sent to the deduper service
    /// or if the response cannot be received.
    pub async fn check_and_add(&self, id: u64) -> Result<bool, DedupError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(DedupCommand::CheckAndAdd(id, response_tx))
            .map_err(DedupError::SendError)?;
        response_rx.await.map_err(DedupError::RecvError)
    }
}

pub struct DedupService {
    deduper: Deduper,
    rx: mpsc::UnboundedReceiver<DedupCommand>,
}

/// A service that handles `check_and_add()` requests without using mutex.
impl DedupService {
    /// Creates a new dedup service with the default capacity (50).
    #[must_use]
    pub fn new() -> (Self, DedupHandle) {
        Self::with_capacity(50)
    }

    /// Creates a new dedup service with the specified capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> (Self, DedupHandle) {
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
                DedupCommand::CheckAndAdd(id, response_tx) => {
                    let was_added = self.deduper.check_and_add(id);
                    if let Err(e) = response_tx.send(was_added) {
                        error!("Failed to send check_and_add response: {e:?}");
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

        // First call should return true (ID was added)
        assert!(handle.check_and_add(123).await.unwrap());

        // Second call should return false (ID already exists)
        assert!(!handle.check_and_add(123).await.unwrap());

        // Different ID should return true again
        assert!(handle.check_and_add(456).await.unwrap());

        // Calling again on already-added IDs should return false
        assert!(!handle.check_and_add(123).await.unwrap());
        assert!(!handle.check_and_add(456).await.unwrap());
    }

    #[tokio::test]
    async fn test_dedup_service_eviction() {
        let (service, handle) = DedupService::with_capacity(3);

        tokio::spawn(async move {
            service.run().await;
        });

        // Add 3 IDs
        assert!(handle.check_and_add(1).await.unwrap());
        assert!(handle.check_and_add(2).await.unwrap());
        assert!(handle.check_and_add(3).await.unwrap());

        // Add a 4th ID, should evict the oldest (1)
        assert!(handle.check_and_add(4).await.unwrap());

        // Now 1 should be addable again (was evicted)
        assert!(handle.check_and_add(1).await.unwrap());

        // But 2 should now be evicted
        assert!(handle.check_and_add(2).await.unwrap());
    }
}
