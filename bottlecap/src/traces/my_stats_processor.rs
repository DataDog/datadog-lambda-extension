use crate::traces::stats_agent::StatsEvent;
use datadog_trace_protobuf::pb;
use tracing::{debug, error};

use crate::config::Config;
use std::sync::Arc;
use crate::tags::provider::Provider as TagProvider;
use crate::traces::stats_concentrator::StatsConcentrator;
use tokio::sync::Mutex;

pub struct MyStatsProcessor {
    config: Arc<Config>,
    resource: String,
    concentrator: Arc<Mutex<StatsConcentrator>>,
}

impl MyStatsProcessor {
    #[must_use]
    pub fn new(config: Arc<Config>, tags_provider: Arc<TagProvider>, stats_concentrator: Arc<Mutex<StatsConcentrator>>) -> Self {
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or(String::from("aws.lambda"));
        Self { config, resource, concentrator: stats_concentrator }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn process(
        &self,
        _event: StatsEvent,
    ) {
        debug!("In my stats processor: Processing stats event.");
        let stats = pb::ClientStatsPayload {
            // hostname: String::new(),
            hostname: "yiming7-hostname".to_string(),
            env: self.config.env.clone().unwrap_or_default(),
            version: self.config.version.clone().unwrap_or_default(),
            lang: "rust".to_string(),
            tracer_version: String::new(),
            runtime_id: String::new(),
            sequence: 0,
            agent_aggregation: String::new(),
            // service: self.config.service.clone().unwrap_or(String::new()),
            service: self.config.service.clone().unwrap_or_default(),
            container_id: String::new(),
            tags: vec![],
            git_commit_sha: String::new(),
            image_tag: String::new(),
            stats: vec![
                pb::ClientStatsBucket {
                    start: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("Invalid time")
                        .as_nanos() as u64,
                    duration: 1_000_000_000,
                    stats: vec![
                        pb::ClientGroupedStats {
                            service: self.config.service.clone().unwrap_or_default(),
                            name: "yiming_name".to_string(),
                            resource: self.resource.clone(),
                            http_status_code: 200,
                            r#type: String::new(),
                            db_type: String::new(),
                            hits: 1,
                            errors: 0,
                            duration: 1_000_000_000,
                            ok_summary: vec![],
                            error_summary: vec![],
                            synthetics: false,
                            top_level_hits: 0,
                            span_kind: String::new(),
                            peer_tags: vec![],
                            is_trace_root: 1,
                        },
                    ],
                    agent_time_shift: 0,
                },
            ],
        };
        self.concentrator.lock().await.add(stats);
    }
}