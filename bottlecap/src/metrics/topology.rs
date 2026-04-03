//! Saluki metrics topology setup.
//!
//! Builds and spawns the Saluki DogStatsD pipeline using bottlecap's
//! already-loaded Config — no re-reading of environment variables.
//!
//! Pipeline: DogStatsD Source + Enhanced Metrics Source → Aggregate → Encoder → Forwarder

use std::sync::Arc;

use figment::providers::Serialized;
use memory_accounting::{ComponentRegistry, MemoryLimiter};
use saluki_components::{
    encoders::DatadogMetricsConfiguration,
    forwarders::DatadogConfiguration,
    sources::DogStatsDConfiguration,
    transforms::{AggregateConfiguration, AggregatorHandle},
};
use saluki_config::ConfigurationLoader;
use saluki_core::topology::{RunningTopology, TopologyBlueprint};
use saluki_error::{ErrorContext as _, GenericError};
use saluki_health::HealthRegistry;
use tracing::debug;

use crate::config::Config;
use crate::metrics::enhanced_source::{EnhancedMetricsHandle, EnhancedMetricsSourceBuilder};

/// Result of starting the Saluki metrics topology.
pub struct MetricsTopology {
    /// Handle for injecting enhanced Lambda metrics into the pipeline.
    pub enhanced_metrics_handle: EnhancedMetricsHandle,
    /// Handle for triggering on-demand flushes (at invocation boundaries).
    pub aggregator_handle: AggregatorHandle,
    /// The running topology — call `shutdown_with_timeout()` on SHUTDOWN.
    pub running: RunningTopology,
}

/// Build a `GenericConfiguration` from bottlecap's already-loaded Config,
/// mapping our config fields to the keys Saluki components expect.
fn build_saluki_config(config: &Config) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    // DogStatsD source config
    map.insert(
        "dogstatsd_port".into(),
        serde_json::json!(crate::DOGSTATSD_PORT),
    );
    if let Some(buf_size) = config.dogstatsd_buffer_size {
        map.insert("dogstatsd_buffer_size".into(), serde_json::json!(buf_size));
    }
    if let Some(so_rcvbuf) = config.dogstatsd_so_rcvbuf {
        map.insert("dogstatsd_so_rcvbuf".into(), serde_json::json!(so_rcvbuf));
    }

    // Forwarder / endpoint config
    map.insert("api_key".into(), serde_json::json!(config.api_key));
    if !config.site.is_empty() {
        map.insert("site".into(), serde_json::json!(config.site));
    }
    if !config.dd_url.is_empty() {
        map.insert("dd_url".into(), serde_json::json!(config.dd_url));
    } else if !config.url.is_empty() {
        map.insert("dd_url".into(), serde_json::json!(config.url));
    }

    serde_json::Value::Object(map)
}

/// Build and spawn the Saluki metrics pipeline from bottlecap's Config.
pub async fn start_metrics_topology(
    config: &Arc<Config>,
) -> Result<MetricsTopology, GenericError> {
    // Initialize Saluki's default root certificate store from the platform's
    // native certificates. Uses rustls-native-certs 0.8.2 + openssl-probe 0.1.6
    // (the fast path that loads a single CA bundle file on Lambda).
    // This is idempotent — safe to call even if already initialized.
    if let Err(e) = saluki_tls::load_platform_root_certificates() {
        tracing::warn!("Failed to load platform root certificates for Saluki TLS: {e}");
    }

    // Build a GenericConfiguration from our already-loaded config values.
    // This avoids re-reading DD_* env vars — we pass what we already have.
    let saluki_values = build_saluki_config(config);
    let generic_config = ConfigurationLoader::default()
        .add_providers([Serialized::defaults(saluki_values)])
        .into_generic()
        .await
        .error_context("Failed to build Saluki configuration from bottlecap Config")?;

    let health_registry = HealthRegistry::new();
    let component_registry = ComponentRegistry::default();

    let mut blueprint = TopologyBlueprint::new("metrics", &component_registry);

    // --- Sources ---

    let dsd_config = DogStatsDConfiguration::from_configuration(&generic_config)
        .error_context("Failed to create DogStatsD source configuration")?;
    blueprint.add_source("dogstatsd", dsd_config)?;

    let (enhanced_source_builder, enhanced_metrics_handle) = EnhancedMetricsSourceBuilder::new();
    blueprint.add_source("enhanced_metrics", enhanced_source_builder)?;

    // --- Transform ---

    let mut agg_config = AggregateConfiguration::from_configuration(&generic_config)
        .unwrap_or_else(|_| AggregateConfiguration::with_defaults());
    let aggregator_handle = agg_config.create_handle();
    blueprint.add_transform("aggregate", agg_config)?;

    // --- Encoder ---

    let encoder_config = DatadogMetricsConfiguration::from_configuration(&generic_config)
        .error_context("Failed to create metrics encoder configuration")?;
    blueprint.add_encoder("metrics_encoder", encoder_config)?;

    // --- Forwarder ---

    let forwarder_config = DatadogConfiguration::from_configuration(&generic_config)
        .error_context("Failed to create Datadog forwarder configuration")?;
    blueprint.add_forwarder("datadog_forwarder", forwarder_config)?;

    // --- Wiring ---
    // DogStatsD source has named outputs: "metrics", "events", "service_checks".
    // We only connect the "metrics" output to the aggregation pipeline.
    blueprint.connect_component("aggregate", ["dogstatsd.metrics", "enhanced_metrics"])?;
    blueprint.connect_component("metrics_encoder", ["aggregate"])?;
    blueprint.connect_component("datadog_forwarder", ["metrics_encoder"])?;

    // --- Build and spawn on the current tokio runtime ---
    let built = blueprint
        .build()
        .await
        .error_context("Failed to build metrics topology")?;

    debug!("Metrics topology built, spawning on current runtime...");

    // Use ambient worker pool to reuse bottlecap's existing tokio runtime,
    // avoiding a dedicated 8-thread pool in Lambda's constrained environment.
    let memory_limiter = MemoryLimiter::noop();
    let running = built
        .with_ambient_worker_pool()
        .spawn(&health_registry, memory_limiter)
        .await
        .error_context("Failed to spawn metrics topology")?;

    debug!("Saluki metrics topology started.");

    Ok(MetricsTopology {
        enhanced_metrics_handle,
        aggregator_handle,
        running,
    })
}
