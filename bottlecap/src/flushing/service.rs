//! `FlushingService` for coordinating flush operations across multiple flusher types.

use std::sync::Arc;

use tracing::{debug, error};

use dogstatsd::{
    aggregator_service::AggregatorHandle as MetricsAggregatorHandle,
    flusher::Flusher as MetricsFlusher,
};

use crate::flushing::handles::{FlushHandles, MetricsRetryBatch};
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
pub struct FlushingService {
    // Flushers
    logs_flusher: LogsFlusher,
    trace_flusher: Arc<TraceFlusher>,
    stats_flusher: Arc<StatsFlusher>,
    proxy_flusher: Arc<ProxyFlusher>,
    metrics_flushers: Arc<Vec<MetricsFlusher>>,

    // Metrics aggregator handle for getting data to flush
    metrics_aggr_handle: MetricsAggregatorHandle,

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
        metrics_flushers: Arc<Vec<MetricsFlusher>>,
        metrics_aggr_handle: MetricsAggregatorHandle,
    ) -> Self {
        Self {
            logs_flusher,
            trace_flusher,
            stats_flusher,
            proxy_flusher,
            metrics_flushers,
            metrics_aggr_handle,
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
    /// This method spawns async tasks for logs, traces, metrics, stats, and proxy flushers.
    /// The tasks run concurrently and their handles are stored for later awaiting.
    ///
    /// For metrics, this first fetches data from the aggregator, then spawns flush tasks
    /// for each metrics flusher (supporting multiple endpoints).
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

        // Spawn metrics flush
        // First get the data from aggregator, then spawn flush tasks for each flusher
        let flush_response = self
            .metrics_aggr_handle
            .clone()
            .flush()
            .await
            .expect("can't flush metrics handle");
        let series = flush_response.series;
        let sketches = flush_response.distributions;

        debug!(
            "FLUSHING_SERVICE | flushing {} series batch(es) and {} sketch batch(es)",
            series.len(),
            sketches.len()
        );
        for (i, sketch_payload) in sketches.iter().enumerate() {
            for sketch in &sketch_payload.sketches {
                debug!(
                    "FLUSHING_SERVICE | sketch batch[{}]: metric='{}' tags={:?}",
                    i, sketch.metric, sketch.tags
                );
            }
        }

        for (idx, flusher) in self.metrics_flushers.iter().enumerate() {
            let flusher = flusher.clone();
            let series_clone = series.clone();
            let sketches_clone = sketches.clone();
            let handle = tokio::spawn(async move {
                let (retry_series, retry_sketches) = flusher
                    .flush_metrics(series_clone, sketches_clone)
                    .await
                    .unwrap_or_default();
                MetricsRetryBatch {
                    flusher_id: idx,
                    series: retry_series,
                    sketches: retry_sketches,
                }
            });
            self.handles.metric_flush_handles.push(handle);
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
    ///
    /// This method:
    /// 1. Drains all pending handles
    /// 2. Awaits each handle's completion
    /// 3. For failed flushes that returned retry data, spawns redrive tasks
    /// 4. Waits for all redrive tasks to complete
    ///
    /// # Returns
    ///
    /// Returns `true` if any flush operation encountered an error.
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

        // Await metrics handles with retry
        for handle in self.handles.metric_flush_handles.drain(..) {
            let mf = self.metrics_flushers.clone();
            match handle.await {
                Ok(retry_batch) => {
                    if !retry_batch.series.is_empty() || !retry_batch.sketches.is_empty() {
                        debug!(
                            "FLUSHING_SERVICE | redriving {:?} series and {:?} sketch payloads",
                            retry_batch.series.len(),
                            retry_batch.sketches.len()
                        );
                        joinset.spawn(async move {
                            if let Some(flusher) = mf.get(retry_batch.flusher_id) {
                                flusher
                                    .flush_metrics(retry_batch.series, retry_batch.sketches)
                                    .await;
                            }
                        });
                    }
                }
                Err(e) => {
                    error!("FLUSHING_SERVICE | redrive metrics error {e:?}");
                }
            }
        }

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
    ///
    /// Flushes logs, metrics (series and distributions), traces, stats, and APM proxy
    /// data in parallel using `tokio::join!`. Unlike `spawn_non_blocking`, this waits
    /// for all flushes to complete before returning.
    ///
    /// The stats flusher respects its normal timing constraints (time-based bucketing),
    /// which may result in some stats being held back until the next flush cycle.
    pub async fn flush_blocking(&self) {
        self.flush_blocking_inner(false).await;
    }

    /// Performs a final blocking flush of all telemetry data before shutdown.
    ///
    /// Flushes logs, metrics (series and distributions), traces, stats, and APM proxy
    /// data in parallel. Unlike `flush_blocking`, this forces the stats flusher to
    /// flush immediately regardless of its normal timing constraints.
    ///
    /// Use this during shutdown when this is the last opportunity to send data.
    pub async fn flush_blocking_final(&self) {
        self.flush_blocking_inner(true).await;
    }

    /// Internal implementation for blocking flush operations.
    ///
    /// Fetches metrics from the aggregator and flushes all data types in parallel.
    async fn flush_blocking_inner(&self, force_stats: bool) {
        let flush_response = self
            .metrics_aggr_handle
            .flush()
            .await
            .expect("can't flush metrics aggr handle");

        let metrics_futures: Vec<_> = self
            .metrics_flushers
            .iter()
            .map(|f| {
                f.flush_metrics(
                    flush_response.series.clone(),
                    flush_response.distributions.clone(),
                )
            })
            .collect();

        tokio::join!(
            self.logs_flusher.flush(None),
            futures::future::join_all(metrics_futures),
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
