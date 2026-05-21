//! `FlushingService` for coordinating flush operations across multiple flusher types.

use std::sync::Arc;

use tracing::{debug, error};

use dogstatsd::{
    aggregator::AggregatorHandle as MetricsAggregatorHandle, flusher::Flusher as MetricsFlusher,
};

use crate::flushing::dlq::Dlq;
use crate::flushing::handles::{FlushHandles, MetricsRetryBatch};
use crate::flushing::{estimate_metrics_batch_size, estimate_stats_size};
use crate::logs::flusher::LogsFlusher;
use crate::traces::{
    proxy_flusher::Flusher as ProxyFlusher, stats_flusher::StatsFlusher,
    trace_flusher::TraceFlusher,
};

const DLQ_BYTES_PER_LOG_REQUEST: u64 = 4_096;
const DLQ_BYTES_PER_PROXY_REQUEST: u64 = 8_192;
const DLQ_BYTES_PER_TRACE: u64 = 1_024;

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

    /// In-memory dead letter queue holding payloads that exhausted both the
    /// per-flusher retry loop and the cross-flusher redrive. Drained at the
    /// start of every subsequent `flush_blocking()` call; what remains at
    /// SHUTDOWN is logged as dropped.
    ///
    /// Behind `Arc` + interior mutability so spawned redrive tasks (which
    /// hold `&self` clones, not `&mut self`) can push without restructuring
    /// the existing flush flow.
    pub(crate) dlq: Arc<Dlq>,
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
        dlq_max_bytes: u64,
    ) -> Self {
        Self {
            logs_flusher,
            trace_flusher,
            stats_flusher,
            proxy_flusher,
            metrics_flushers,
            metrics_aggr_handle,
            handles: FlushHandles::new(),
            dlq: Dlq::new(dlq_max_bytes),
        }
    }

    /// Test/inspection accessor for the shared DLQ.
    #[must_use]
    pub fn dlq(&self) -> Arc<Dlq> {
        Arc::clone(&self.dlq)
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
                        let dlq = Arc::clone(&self.dlq);
                        joinset.spawn(async move {
                            if let Some(still_failed) = sf.flush(false, Some(retry)).await {
                                for payload in still_failed {
                                    let size = estimate_stats_size(&payload);
                                    dlq.try_push_stats(payload, size).await;
                                }
                            }
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
                        let dlq = Arc::clone(&self.dlq);
                        joinset.spawn(async move {
                            if let Some(still_failed) = tf.flush(Some(retry)).await {
                                for payload in still_failed {
                                    let size = (payload.len() as u64)
                                        .saturating_mul(DLQ_BYTES_PER_TRACE);
                                    dlq.try_push_traces(payload, size).await;
                                }
                            }
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
                        let dlq = Arc::clone(&self.dlq);
                        match item.try_clone() {
                            Some(item_clone) => {
                                joinset.spawn(async move {
                                    let still_failed = lf.flush(Some(item_clone)).await;
                                    for rb in still_failed {
                                        dlq.try_push_logs(rb, DLQ_BYTES_PER_LOG_REQUEST).await;
                                    }
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
                        let dlq = Arc::clone(&self.dlq);
                        joinset.spawn(async move {
                            if let Some(flusher) = mf.get(retry_batch.flusher_id) {
                                let flusher_id = retry_batch.flusher_id;
                                let (still_series, still_sketches) = flusher
                                    .flush_metrics(retry_batch.series, retry_batch.sketches)
                                    .await
                                    .unwrap_or_default();
                                let still_batch = MetricsRetryBatch {
                                    flusher_id,
                                    series: still_series,
                                    sketches: still_sketches,
                                };
                                if !still_batch.series.is_empty()
                                    || !still_batch.sketches.is_empty()
                                {
                                    let size = estimate_metrics_batch_size(&still_batch);
                                    dlq.try_push_metrics(still_batch, size).await;
                                }
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
                    let dlq = Arc::clone(&self.dlq);
                    joinset.spawn(async move {
                        if let Some(still_failed) = pf.flush(Some(batch)).await {
                            for rb in still_failed {
                                dlq.try_push_proxy(rb, DLQ_BYTES_PER_PROXY_REQUEST).await;
                            }
                        }
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

    /// Drains every DLQ queue and re-flushes the payloads. Items that still fail
    /// are pushed back into the DLQ (subject to the byte cap). Called at the top
    /// of every `flush_blocking_inner` so the next invocation's flush always
    /// attempts any previously deferred payloads before pulling new data.
    async fn drain_dlq(&self) {
        // ── Logs ──────────────────────────────────────────────────────────────
        let log_items: Vec<_> = { self.dlq.logs.lock().await.drain(..).collect() };
        for item in log_items {
            self.dlq.release(item.size_bytes);
            for rb in self.logs_flusher.flush(Some(item.payload)).await {
                self.dlq.try_push_logs(rb, DLQ_BYTES_PER_LOG_REQUEST).await;
            }
        }

        // ── Traces ────────────────────────────────────────────────────────────
        let trace_items: Vec<_> = { self.dlq.traces.lock().await.drain(..).collect() };
        if !trace_items.is_empty() {
            let total_bytes: u64 = trace_items.iter().map(|i| i.size_bytes).sum();
            self.dlq.release(total_bytes);
            let payloads = trace_items.into_iter().map(|i| i.payload).collect::<Vec<_>>();
            if let Some(still_failed) = self.trace_flusher.flush(Some(payloads)).await {
                for payload in still_failed {
                    let size = (payload.len() as u64).saturating_mul(DLQ_BYTES_PER_TRACE);
                    self.dlq.try_push_traces(payload, size).await;
                }
            }
        }

        // ── Stats ─────────────────────────────────────────────────────────────
        let stats_items: Vec<_> = { self.dlq.stats.lock().await.drain(..).collect() };
        if !stats_items.is_empty() {
            let total_bytes: u64 = stats_items.iter().map(|i| i.size_bytes).sum();
            self.dlq.release(total_bytes);
            let payloads = stats_items.into_iter().map(|i| i.payload).collect::<Vec<_>>();
            if let Some(still_failed) = self.stats_flusher.flush(false, Some(payloads)).await {
                for payload in still_failed {
                    let size = estimate_stats_size(&payload);
                    self.dlq.try_push_stats(payload, size).await;
                }
            }
        }

        // ── Proxy ─────────────────────────────────────────────────────────────
        let proxy_items: Vec<_> = { self.dlq.proxy.lock().await.drain(..).collect() };
        if !proxy_items.is_empty() {
            let total_bytes: u64 = proxy_items.iter().map(|i| i.size_bytes).sum();
            self.dlq.release(total_bytes);
            let payloads = proxy_items.into_iter().map(|i| i.payload).collect::<Vec<_>>();
            if let Some(still_failed) = self.proxy_flusher.flush(Some(payloads)).await {
                for rb in still_failed {
                    self.dlq.try_push_proxy(rb, DLQ_BYTES_PER_PROXY_REQUEST).await;
                }
            }
        }

        // ── Metrics ───────────────────────────────────────────────────────────
        let metrics_items: Vec<_> = { self.dlq.metrics.lock().await.drain(..).collect() };
        for item in metrics_items {
            self.dlq.release(item.size_bytes);
            let MetricsRetryBatch { flusher_id, series, sketches } = item.payload;
            if let Some(flusher) = self.metrics_flushers.get(flusher_id) {
                let (still_series, still_sketches) = flusher
                    .flush_metrics(series, sketches)
                    .await
                    .unwrap_or_default();
                let still_batch = MetricsRetryBatch {
                    flusher_id,
                    series: still_series,
                    sketches: still_sketches,
                };
                if !still_batch.series.is_empty() || !still_batch.sketches.is_empty() {
                    let size = estimate_metrics_batch_size(&still_batch);
                    self.dlq.try_push_metrics(still_batch, size).await;
                }
            }
        }
    }

    /// Internal implementation for blocking flush operations.
    ///
    /// Fetches metrics from the aggregator and flushes all data types in parallel.
    async fn flush_blocking_inner(&self, force_stats: bool) {
        self.drain_dlq().await;

        let total_start = std::time::Instant::now();

        let aggr_start = std::time::Instant::now();
        let flush_response = self
            .metrics_aggr_handle
            .flush()
            .await
            .expect("can't flush metrics aggr handle");
        let aggr_ms = aggr_start.elapsed().as_millis();

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

        let logs_fut = async {
            let s = std::time::Instant::now();
            self.logs_flusher.flush(None).await;
            s.elapsed().as_millis()
        };
        let metrics_fut = async {
            let s = std::time::Instant::now();
            futures::future::join_all(metrics_futures).await;
            s.elapsed().as_millis()
        };
        let trace_fut = async {
            let s = std::time::Instant::now();
            self.trace_flusher.flush(None).await;
            s.elapsed().as_millis()
        };
        let stats_fut = async {
            let s = std::time::Instant::now();
            self.stats_flusher.flush(force_stats, None).await;
            s.elapsed().as_millis()
        };
        let proxy_fut = async {
            let s = std::time::Instant::now();
            self.proxy_flusher.flush(None).await;
            s.elapsed().as_millis()
        };

        let (logs_ms, metrics_ms, trace_ms, stats_ms, proxy_ms) =
            tokio::join!(logs_fut, metrics_fut, trace_fut, stats_fut, proxy_fut);

        debug!(
            "FLUSH_TIMING total={}ms aggr={}ms logs={}ms metrics={}ms trace={}ms stats={}ms proxy={}ms force_stats={}",
            total_start.elapsed().as_millis(),
            aggr_ms,
            logs_ms,
            metrics_ms,
            trace_ms,
            stats_ms,
            proxy_ms,
            force_stats
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
