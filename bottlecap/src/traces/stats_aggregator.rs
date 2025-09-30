use crate::traces::stats_concentrator_service::StatsConcentratorHandle;
use datadog_trace_protobuf::pb::ClientStatsPayload;
use std::collections::VecDeque;
use tracing::error;

#[allow(clippy::empty_line_after_doc_comments)]
/// Maximum number of entries in a stat payload.
///
/// <https://github.com/DataDog/datadog-agent/blob/996dd54337908a6511948fabd2a41420ba919a8b/pkg/trace/writer/stats.go#L35-L41>
// const MAX_BATCH_ENTRIES_SIZE: usize = 4000;

// Aproximate size an entry in a stat payload occupies
//
// <https://github.com/DataDog/datadog-agent/blob/996dd54337908a6511948fabd2a41420ba919a8b/pkg/trace/writer/stats.go#L33-L35>
// const MAX_ENTRY_SIZE_BYTES: usize = 375;

/// Maximum content size per payload in compressed bytes,
///
/// <https://github.com/DataDog/datadog-agent/blob/996dd54337908a6511948fabd2a41420ba919a8b/pkg/trace/writer/stats.go#L35-L41>
const MAX_CONTENT_SIZE_BYTES: usize = 3 * 1024 * 1024; // ~3MB

#[allow(clippy::module_name_repetitions)]
pub struct StatsAggregator {
    queue: VecDeque<ClientStatsPayload>,
    max_content_size_bytes: usize,
    buffer: Vec<ClientStatsPayload>,
    concentrator: StatsConcentratorHandle,
}

/// Takes in individual trace stats payloads and aggregates them into batches to be flushed to Datadog.
impl StatsAggregator {
    #[allow(dead_code)]
    #[allow(clippy::must_use_candidate)]
    fn new(max_content_size_bytes: usize, concentrator: StatsConcentratorHandle) -> Self {
        StatsAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes,
            buffer: Vec::new(),
            concentrator,
        }
    }

    #[must_use]
    pub fn new_with_concentrator(concentrator: StatsConcentratorHandle) -> Self {
        Self::new(MAX_CONTENT_SIZE_BYTES, concentrator)
    }

    /// Takes in an individual trace stats payload.
    pub fn add(&mut self, payload: ClientStatsPayload) {
        self.queue.push_back(payload);
    }

    /// Returns a batch of trace stats payloads, subject to the max content size.
    pub async fn get_batch(&mut self, force_flush: bool) -> Vec<ClientStatsPayload> {
        // Pull stats data from concentrator
        match self.concentrator.flush(force_flush).await {
            Ok(stats) => {
                self.queue.extend(stats);
            }
            Err(e) => {
                error!("Error getting stats from the stats concentrator: {e:?}");
            }
        }

        let mut batch_size = 0;

        // Fill the batch
        while batch_size < self.max_content_size_bytes {
            if let Some(payload) = self.queue.pop_front() {
                let payload_size = size_of_val(&payload);

                // Put stats back in the queue
                if batch_size + payload_size > self.max_content_size_bytes {
                    self.queue.push_front(payload);
                    break;
                }
                batch_size += payload_size;
                self.buffer.push(payload);
            } else {
                break;
            }
        }

        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::LAMBDA_RUNTIME_SLUG;
    use crate::config::Config;
    use crate::tags::provider::Provider as TagProvider;
    use crate::traces::stats_concentrator_service::StatsConcentratorService;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn test_add() {
        let config = Arc::new(Config::default());
        let tags_provider = Arc::new(TagProvider::new(
            config.clone(),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let (_, concentrator) = StatsConcentratorService::new(config, tags_provider);
        let mut aggregator = StatsAggregator::new_with_concentrator(concentrator);
        let payload = ClientStatsPayload {
            hostname: "hostname".to_string(),
            env: "dev".to_string(),
            version: "version".to_string(),
            stats: vec![],
            lang: "rust".to_string(),
            tracer_version: "tracer.version".to_string(),
            runtime_id: "hash".to_string(),
            sequence: 0,
            agent_aggregation: "aggregation".to_string(),
            service: "service".to_string(),
            container_id: "container_id".to_string(),
            tags: vec![],
            git_commit_sha: "git_commit_sha".to_string(),
            image_tag: "image_tag".to_string(),
        };

        aggregator.add(payload.clone());
        assert_eq!(aggregator.queue.len(), 1);
        assert_eq!(aggregator.queue[0], payload);
    }

    #[tokio::test]
    async fn test_get_batch() {
        let config = Arc::new(Config::default());
        let tags_provider = Arc::new(TagProvider::new(
            config.clone(),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let (_, concentrator) = StatsConcentratorService::new(config, tags_provider);
        let mut aggregator = StatsAggregator::new_with_concentrator(concentrator);
        let payload = ClientStatsPayload {
            hostname: "hostname".to_string(),
            env: "dev".to_string(),
            version: "version".to_string(),
            stats: vec![],
            lang: "rust".to_string(),
            tracer_version: "tracer.version".to_string(),
            runtime_id: "hash".to_string(),
            sequence: 0,
            agent_aggregation: "aggregation".to_string(),
            service: "service".to_string(),
            container_id: "container_id".to_string(),
            tags: vec![],
            git_commit_sha: "git_commit_sha".to_string(),
            image_tag: "image_tag".to_string(),
        };
        aggregator.add(payload.clone());
        assert_eq!(aggregator.queue.len(), 1);
        let batch = aggregator.get_batch(false).await;
        assert_eq!(batch, vec![payload]);
    }

    #[tokio::test]
    async fn test_get_batch_full_entries() {
        let config = Arc::new(Config::default());
        let tags_provider = Arc::new(TagProvider::new(
            config.clone(),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let (_, concentrator) = StatsConcentratorService::new(config, tags_provider);
        let mut aggregator = StatsAggregator::new(640, concentrator);
        // Payload below is 115 bytes
        let payload = ClientStatsPayload {
            hostname: "hostname".to_string(),
            env: "dev".to_string(),
            version: "version".to_string(),
            stats: vec![],
            lang: "rust".to_string(),
            tracer_version: "tracer.version".to_string(),
            runtime_id: "hash".to_string(),
            sequence: 0,
            agent_aggregation: "aggregation".to_string(),
            service: "service".to_string(),
            container_id: "container_id".to_string(),
            tags: vec![],
            git_commit_sha: "git_commit_sha".to_string(),
            image_tag: "image_tag".to_string(),
        };

        // Add 3 payloads
        aggregator.add(payload.clone());
        aggregator.add(payload.clone());
        aggregator.add(payload.clone());

        // The batch should only contain the first 2 payloads
        let first_batch = aggregator.get_batch(false).await;
        assert_eq!(first_batch, vec![payload.clone(), payload.clone()]);
        assert_eq!(aggregator.queue.len(), 1);

        // The second batch should only contain the last log
        let second_batch = aggregator.get_batch(false).await;
        assert_eq!(second_batch, vec![payload]);
        assert_eq!(aggregator.queue.len(), 0);
    }
}
