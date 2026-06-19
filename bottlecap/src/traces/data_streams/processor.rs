//! Extension-side DSM consume processor.
//!
//! Owns the checkpoint [`Aggregator`] and bridges it to the existing proxy
//! flush path: consume checkpoints are folded in during invocation start, and
//! on flush the aggregated pipeline-stats payload is gzipped and enqueued as a
//! [`ProxyRequest`] so the shared [`crate::traces::proxy_flusher`] ships it to
//! `/api/v0.1/pipeline_stats` (adding the API key + tags).
//!
//! Gated entirely by `DD_DATA_STREAMS_ENABLED`; when disabled this is never
//! constructed.

use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use flate2::Compression;
use flate2::write::GzEncoder;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, warn};

use crate::traces::data_streams::aggregator::Aggregator;
use crate::traces::data_streams::checkpoint::compute_consume_checkpoint;
use crate::traces::data_streams::context::{
    DD_PATHWAY_CTX_BASE64_KEY, DD_PATHWAY_CTX_KEY, DsmContext,
};
use crate::traces::proxy_aggregator::{Aggregator as ProxyAggregator, ProxyRequest};

/// gzip level used by the tracer for pipeline stats.
const GZIP_LEVEL: u32 = 1;

pub struct DsmProcessor {
    service: String,
    env: String,
    aggregator: Mutex<Aggregator>,
    proxy_aggregator: Arc<TokioMutex<ProxyAggregator>>,
    target_url: String,
}

impl DsmProcessor {
    #[must_use]
    pub fn new(
        service: String,
        env: String,
        tracer_version: String,
        site: &str,
        proxy_aggregator: Arc<TokioMutex<ProxyAggregator>>,
    ) -> Self {
        let aggregator = Aggregator::new(service.clone(), env.clone(), tracer_version);
        Self {
            service,
            env,
            aggregator: Mutex::new(aggregator),
            proxy_aggregator,
            target_url: format!("https://trace.agent.{site}/api/v0.1/pipeline_stats"),
        }
    }

    /// Record a consume (`direction:in`) checkpoint for an inbound event.
    ///
    /// `edge_tags` come from the trigger (`Trigger::get_dsm_edge_tags`); `carrier`
    /// is the trigger carrier (which may contain the inbound pathway context).
    pub fn record_consume(
        &self,
        edge_tags: &[String],
        carrier: &std::collections::HashMap<String, String>,
        payload_size: f64,
    ) {
        let ctx = extract_pathway_context(carrier);
        let now_ns = now_unix_nanos();

        let checkpoint = compute_consume_checkpoint(
            &self.service,
            &self.env,
            edge_tags,
            ctx.as_ref(),
            now_ns,
            None,
        );

        debug!(
            "DSM: recorded consume checkpoint hash={:x} parent={:x} has_inbound_ctx={} edge_tags={:?}",
            u64::from_le_bytes(checkpoint.hash),
            u64::from_le_bytes(checkpoint.parent_hash),
            ctx.is_some(),
            edge_tags
        );

        match self.aggregator.lock() {
            Ok(mut agg) => agg.add(&checkpoint, payload_size),
            Err(e) => warn!("DSM: aggregator lock poisoned; dropping consume checkpoint: {e}"),
        }
    }

    /// Drain the aggregator into the proxy aggregator for flushing. No-op when
    /// there is nothing buffered.
    pub async fn drain_into_proxy(&self) {
        let payload = match self.aggregator.lock() {
            Ok(mut agg) => agg.take_payload(),
            Err(e) => {
                warn!("DSM: aggregator lock poisoned; skipping pipeline stats flush: {e}");
                return;
            }
        };
        let Some(payload) = payload else {
            return;
        };

        let body = match gzip(&payload) {
            Ok(b) => b,
            Err(e) => {
                warn!("DSM: failed to gzip pipeline stats payload: {e}");
                return;
            }
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/msgpack"),
        );
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));

        let request = ProxyRequest {
            headers,
            body: Bytes::from(body),
            target_url: self.target_url.clone(),
        };

        debug!(
            "DSM: enqueued pipeline stats payload ({} bytes gzipped)",
            request.body.len()
        );
        self.proxy_aggregator.lock().await.add(request);
    }
}

/// Extract the inbound DSM pathway context from a carrier, preferring the
/// base64 key. Fails closed (returns `None`) on malformed input.
fn extract_pathway_context(
    carrier: &std::collections::HashMap<String, String>,
) -> Option<DsmContext> {
    carrier
        .get(DD_PATHWAY_CTX_BASE64_KEY)
        .or_else(|| carrier.get(DD_PATHWAY_CTX_KEY))
        .and_then(|v| DsmContext::from_base64(v))
}

fn now_unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(GZIP_LEVEL));
    encoder.write_all(data)?;
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn extracts_base64_context_from_carrier() {
        let mut carrier = HashMap::new();
        carrier.insert(
            DD_PATHWAY_CTX_BASE64_KEY.to_string(),
            "Z7CzXmXArPrE58Cfj2LI2cOfj2I=".to_string(),
        );
        let ctx = extract_pathway_context(&carrier).expect("context");
        assert_eq!(hex::encode(ctx.hash), "67b0b35e65c0acfa");
    }

    #[test]
    fn missing_context_returns_none() {
        let carrier = HashMap::new();
        assert!(extract_pathway_context(&carrier).is_none());
    }

    #[test]
    fn malformed_context_returns_none() {
        let mut carrier = HashMap::new();
        carrier.insert(DD_PATHWAY_CTX_BASE64_KEY.to_string(), "@@bad@@".to_string());
        assert!(extract_pathway_context(&carrier).is_none());
    }

    #[tokio::test]
    async fn drain_enqueues_proxy_request_when_data_present() {
        let proxy = Arc::new(TokioMutex::new(ProxyAggregator::default()));
        let dsm = DsmProcessor::new(
            "svc".into(),
            "env".into(),
            "1.0".into(),
            "datadoghq.com",
            proxy.clone(),
        );

        let edge_tags = vec![
            "direction:in".to_string(),
            "topic:q".to_string(),
            "type:sqs".to_string(),
        ];
        dsm.record_consume(&edge_tags, &HashMap::new(), 128.0);
        dsm.drain_into_proxy().await;

        let batch = proxy.lock().await.get_batch();
        assert_eq!(batch.len(), 1);
        assert!(batch[0].target_url.ends_with("/api/v0.1/pipeline_stats"));
    }

    #[tokio::test]
    async fn drain_is_noop_when_empty() {
        let proxy = Arc::new(TokioMutex::new(ProxyAggregator::default()));
        let dsm = DsmProcessor::new(
            "svc".into(),
            "env".into(),
            "1.0".into(),
            "datadoghq.com",
            proxy.clone(),
        );
        dsm.drain_into_proxy().await;
        assert_eq!(proxy.lock().await.get_batch().len(), 0);
    }
}
