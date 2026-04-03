//! `FlushingService` for coordinating flush operations across multiple flusher types.

use std::sync::Arc;

use tracing::{debug, error};

use saluki_components::transforms::AggregatorHandle;

use crate::flushing::handles::FlushHandles;
use crate::logs::flusher::LogsFlusher;
use crate::traces::{
    proxy_flusher::Flusher as ProxyFlusher, stats_flusher::StatsFlusher,
    trace_flusher::TraceFlusher,
};

/// Service for coordinating flush operations across all flusher types.
///
/// This service provides a unified interface for:
/// - Spawning non-blocking flush tasks
/// - Awaiting pending flush handles with retry logic
/// - Performing blocking flushes (spawn + await)
///
/// Metrics flushing is handled by the Saluki topology — the `AggregatorHandle`
/// triggers an on-demand flush which pushes data through the encoder and forwarder.
pub struct FlushingService {
    // Flushers
    logs_flusher: LogsFlusher,
    trace_flusher: Arc<TraceFlusher>,
    stats_flusher: Arc<StatsFlusher>,
    proxy_flusher: Arc<ProxyFlusher>,

    // Saluki aggregator handle for triggering on-demand metric flushes.
    // The actual encoding + HTTP shipping happens inside the Saluki topology.
    saluki_aggr_handle: AggregatorHandle,

    // Pending flush handles
    handles: FlushHandles,
}

impl FlushingService {
    /// Creates a new `FlushingService` with the given flushers.
    #[must_use]
    pub fn new(
        logs_flusher: LogsFlusher,
        trace_flusher: Arc<TraceFlusher>,
        stats_flusher: Arc<StatsFlusher>,
        proxy_flusher: Arc<ProxyFlusher>,
        saluki_aggr_handle: AggregatorHandle,
    ) -> Self {
        Self {
            logs_flusher,
            trace_flusher,
            stats_flusher,
            proxy_flusher,
            saluki_aggr_handle,
            handles: FlushHandles::new(),
        }
    }

    /// Returns `true` if any flush operation is still pending.
    #[must_use]
    pub fn has_pending_handles(&self) -> bool {
        self.handles.has_pending()
    }

    /// Spawns non-blocking flush tasks for all flushers.
    ///
    /// For metrics, this triggers the Saluki aggregator to flush — the data
    /// flows through the topology's encoder and forwarder automatically.
    pub async fn spawn_non_blocking(&mut self) {
        // Spawn logs flush
        let lf = self.logs_flusher.clone();
        self.handles
            .log_flush_handles
            .push(tokio::spawn(async move { lf.flush(None).await }));

        // Spawn traces flush
        let tf = self.trace_flusher.clone();
        self.handles
            .trace_flush_handles
            .push(tokio::spawn(async move {
                tf.flush(None).await.unwrap_or_default()
            }));

        // Trigger Saluki metrics flush (encoding + shipping handled by the topology)
        if let Err(e) = self.saluki_aggr_handle.flush().await {
            error!("FLUSHING_SERVICE | metrics flush error: {e}");
        }

        // Spawn stats flush
        let sf = Arc::clone(&self.stats_flusher);
        self.handles
            .stats_flush_handles
            .push(tokio::spawn(async move {
                sf.flush(false, None).await.unwrap_or_default()
            }));

        // Spawn proxy flush
        let pf = self.proxy_flusher.clone();
        self.handles
            .proxy_flush_handles
            .push(tokio::spawn(async move {
                pf.flush(None).await.unwrap_or_default()
            }));
    }

    /// Awaits all pending flush handles and performs retry for failed flushes.
    #[allow(clippy::too_many_lines)]
    pub async fn await_handles(&mut self) -> bool {
        let mut joinset = tokio::task::JoinSet::new();
        let mut flush_error = false;

        // Await stats handles with retry
        for handle in self.handles.stats_flush_handles.drain(..) {
            match handle.await {
                Ok(retry) => {
                    let sf = self.stats_flusher.clone();
                    if !retry.is_empty() {
                        debug!(
                            "FLUSHING_SERVICE | redriving {:?} stats payloads",
                            retry.len()
                        );
                        joinset.spawn(async move {
                            sf.flush(false, Some(retry)).await;
                        });
                    }
                }
                Err(e) => {
                    error!("FLUSHING_SERVICE | stats flush error {e:?}");
                    flush_error = true;
                }
            }
        }

        // Await trace handles with retry
        for handle in self.handles.trace_flush_handles.drain(..) {
            match handle.await {
                Ok(retry) => {
                    let tf = self.trace_flusher.clone();
                    if !retry.is_empty() {
                        debug!(
                            "FLUSHING_SERVICE | redriving {:?} trace payloads",
                            retry.len()
                        );
                        joinset.spawn(async move {
                            tf.flush(Some(retry)).await;
                        });
                    }
                }
                Err(e) => {
                    error!("FLUSHING_SERVICE | redrive trace error {e:?}");
                }
            }
        }

        // Await log handles with retry
        for handle in self.handles.log_flush_handles.drain(..) {
            match handle.await {
                Ok(retry) => {
                    if !retry.is_empty() {
                        debug!(
                            "FLUSHING_SERVICE | redriving {:?} log payloads",
                            retry.len()
                        );
                    }
                    for item in retry {
                        let lf = self.logs_flusher.clone();
                        match item.try_clone() {
                            Some(item_clone) => {
                                joinset.spawn(async move {
                                    lf.flush(Some(item_clone)).await;
                                });
                            }
                            None => {
                                error!("FLUSHING_SERVICE | Can't clone redrive log payloads");
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("FLUSHING_SERVICE | redrive log error {e:?}");
                }
            }
        }

        // Metrics: no retry handles — Saluki's forwarder handles retries internally
        // with exponential backoff and circuit breaker.

        // Await proxy handles with retry
        for handle in self.handles.proxy_flush_handles.drain(..) {
            match handle.await {
                Ok(batch) => {
                    if !batch.is_empty() {
                        debug!(
                            "FLUSHING_SERVICE | Redriving {:?} APM proxy payloads",
                            batch.len()
                        );
                    }

                    let pf = self.proxy_flusher.clone();
                    joinset.spawn(async move {
                        pf.flush(Some(batch)).await;
                    });
                }
                Err(e) => {
                    error!("FLUSHING_SERVICE | Redrive error in APM proxy: {e:?}");
                }
            }
        }

        // Wait for all redrive operations to complete
        while let Some(result) = joinset.join_next().await {
            if let Err(e) = result {
                error!("FLUSHING_SERVICE | redrive request error {e:?}");
                flush_error = true;
            }
        }

        flush_error
    }

    /// Performs a blocking flush of all telemetry data.
    pub async fn flush_blocking(&self) {
        self.flush_blocking_inner(false).await;
    }

    /// Performs a final blocking flush of all telemetry data before shutdown.
    pub async fn flush_blocking_final(&self) {
        self.flush_blocking_inner(true).await;
    }

    /// Internal implementation for blocking flush operations.
    async fn flush_blocking_inner(&self, force_stats: bool) {
        // Trigger Saluki metrics flush (encoding + shipping handled by the topology)
        let metrics_flush = self.saluki_aggr_handle.flush();

        tokio::join!(
            self.logs_flusher.flush(None),
            metrics_flush,
            self.trace_flusher.flush(None),
            self.stats_flusher.flush(force_stats, None),
            self.proxy_flusher.flush(None),
        );
    }
}

impl std::fmt::Debug for FlushingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlushingService")
            .field("handles", &self.handles)
            .finish_non_exhaustive()
    }
}
