//! Wiring for the logs pipeline, shared by `main.rs` and the logs contract tests so both
//! exercise exactly the same aggregator, agent and flusher setup.

use std::sync::Arc;

use dogstatsd::api_key::ApiKeyFactory;
use reqwest::Client;
use tokio::sync::mpsc::Sender;

use crate::config::Config;
use crate::event_bus::Event;
use crate::extension::telemetry::events::TelemetryEvent;
use crate::logs::agent::LogsAgent;
use crate::logs::aggregator_service::{AggregatorHandle, AggregatorService};
use crate::logs::flusher::LogsFlusher;
use crate::logs::lambda::DurableContextUpdate;
use crate::tags::provider::Provider as TagProvider;

/// The pieces of a logs pipeline. The aggregator task is already running; the agent is
/// returned unspawned so the caller decides how to drive it: `main.rs` spawns `spin()`,
/// tests call `sync_consume()`.
pub struct LogsPipeline {
    pub agent: LogsAgent,
    pub telemetry_tx: Sender<TelemetryEvent>,
    pub durable_context_tx: Sender<DurableContextUpdate>,
    pub flusher: LogsFlusher,
    pub aggregator_handle: AggregatorHandle,
}

/// Creates the logs aggregator (and starts its background task), the logs agent and the
/// flusher. Must be called from within a tokio runtime.
#[must_use]
pub fn build_logs_pipeline(
    config: &Arc<Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    tags_provider: &Arc<TagProvider>,
    event_bus: Sender<Event>,
    is_managed_instance_mode: bool,
    client: &Client,
) -> LogsPipeline {
    let (aggregator_service, aggregator_handle) = AggregatorService::default();
    tokio::spawn(async move {
        aggregator_service.run().await;
    });

    let (agent, telemetry_tx, durable_context_tx) = LogsAgent::new(
        Arc::clone(tags_provider),
        Arc::clone(config),
        event_bus,
        aggregator_handle.clone(),
        is_managed_instance_mode,
    );

    let flusher = LogsFlusher::new(
        api_key_factory,
        aggregator_handle.clone(),
        Arc::clone(config),
        client.clone(),
    );

    LogsPipeline {
        agent,
        telemetry_tx,
        durable_context_tx,
        flusher,
        aggregator_handle,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::LAMBDA_RUNTIME_SLUG;
    use crate::event_bus::EventBus;
    use std::collections::HashMap;

    #[tokio::test]
    async fn build_logs_pipeline_routes_telemetry_to_the_aggregator() {
        let config = Arc::new(Config::default());
        let tags_provider = Arc::new(TagProvider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));
        let (_event_bus, event_bus_tx) = EventBus::run();
        let client = crate::http::get_client(&config);

        // Managed Instance mode ships logs without a request_id immediately,
        // so a single function log is enough to reach the aggregator.
        let mut pipeline = build_logs_pipeline(
            &config,
            Arc::new(ApiKeyFactory::new("test-key")),
            &tags_provider,
            event_bus_tx,
            true,
            &client,
        );
        let event: TelemetryEvent = serde_json::from_str(
            r#"{"time":"2024-04-24T12:34:56.789Z","type":"function","record":"hello"}"#,
        )
        .unwrap();
        pipeline.telemetry_tx.send(event).await.unwrap();
        pipeline.agent.sync_consume().await;

        let batches = pipeline.aggregator_handle.get_batches().await.unwrap();
        assert_eq!(batches.len(), 1);
        let logs: serde_json::Value = serde_json::from_slice(&batches[0]).unwrap();
        assert_eq!(logs[0]["message"]["message"], "hello");
        assert_eq!(logs[0]["hostname"], "test-arn");
    }
}
