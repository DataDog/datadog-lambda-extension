/**
 * TODO:
 * 
 */

use datadog_trace_protobuf::pb;
use crate::traces::stats_agent::StatsEvent;
use crate::config::Config;
use std::sync::Arc;
use crate::tags::provider::Provider as TagProvider;


pub struct StatsConcentrator {
    pub storage: Vec<StatsEvent>,
    pub config: Arc<Config>,
    pub resource: String,
}

impl StatsConcentrator {
    #[must_use]
    pub fn new(config: Arc<Config>, tags_provider: Arc<TagProvider>) -> Self {
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or(String::from("aws.lambda"));
        Self { storage: Vec::new(), config, resource }
    }

    pub fn add(&mut self, stats_event: StatsEvent) {
        // debug!("StatsConcentrator | adding stats payload to concentrator: {stats:?}");
        self.storage.push(stats_event);
    }

    #[must_use]
    pub fn get_batch(&mut self) -> Vec<pb::ClientStatsPayload> {
        let ret = self.storage.iter().map(|stats_event| self.construct_stats_payload(stats_event)).collect();
        self.storage.clear();
        ret
    }

    fn construct_stats_payload(&self, stats_event: &StatsEvent) -> pb::ClientStatsPayload {
        pb::ClientStatsPayload {
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
                    start: stats_event.time,
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
        }
    }
}