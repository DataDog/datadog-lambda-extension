//! Flushing service module for coordinating data flushes across multiple flusher types.
//!
//! This module provides a unified `FlushingService` that orchestrates flushing of:
//! - Logs (via `LogsFlusher`)
//! - Traces (via `TraceFlusher`)
//! - Stats (via `StatsFlusher`)
//! - Metrics (via `MetricsFlusher`)
//! - Proxy requests (via `ProxyFlusher`)

mod handles;
mod service;

pub use handles::{FlushHandles, MetricsRetryBatch};
pub use service::FlushingService;
