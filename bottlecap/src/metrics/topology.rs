//! Saluki metrics topology setup.
//!
//! Builds and spawns the Saluki DogStatsD pipeline.
//! Pipeline: DogStatsD Source + Enhanced Metrics Source → Aggregate → Encoder → Forwarder
//!
//! Saluki components are configured from DD_* environment variables — the same
//! env vars that bottlecap reads. Since they're already in the process
//! environment, Saluki's ConfigurationLoader reads them directly.

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

/// Build and spawn the Saluki metrics pipeline.
///
/// Configuration is loaded from DD_* environment variables (the same env vars
/// bottlecap uses). Saluki components read `DD_API_KEY`, `DD_SITE`, `DD_URL`,
/// `DD_DOGSTATSD_PORT`, etc. directly from the process environment.
pub async fn start_metrics_topology() -> Result<MetricsTopology, GenericError> {
    let config = ConfigurationLoader::default()
        .from_environment("DD")
        .error_context("Failed to load DD_* environment variables")?
        .into_generic()
        .await
        .error_context("Failed to build Saluki configuration from environment")?;

    let health_registry = HealthRegistry::new();
    let component_registry = ComponentRegistry::default();

    let mut blueprint = TopologyBlueprint::new("metrics", &component_registry);

    // --- Sources ---

    // DogStatsD UDP source (reads DD_DOGSTATSD_PORT, defaults to 8125)
    let dsd_config = DogStatsDConfiguration::from_configuration(&config)
        .error_context("Failed to create DogStatsD source configuration")?;
    blueprint.add_source("dogstatsd", dsd_config)?;

    // Enhanced metrics source (channel-based, for programmatic Lambda metric injection)
    let (enhanced_source_builder, enhanced_metrics_handle) = EnhancedMetricsSourceBuilder::new();
    blueprint.add_source("enhanced_metrics", enhanced_source_builder)?;

    // --- Transform ---

    // Aggregation with on-demand flush handle for Lambda invocation boundaries
    let mut agg_config = AggregateConfiguration::from_configuration(&config)
        .unwrap_or_else(|_| AggregateConfiguration::with_defaults());
    let aggregator_handle = agg_config.create_handle();
    blueprint.add_transform("aggregate", agg_config)?;

    // --- Encoder ---

    // Protobuf encoder for both /api/v2/series and /api/beta/sketches
    let encoder_config = DatadogMetricsConfiguration::from_configuration(&config)
        .error_context("Failed to create metrics encoder configuration")?;
    blueprint.add_encoder("metrics_encoder", encoder_config)?;

    // --- Forwarder ---

    // HTTP forwarder with retries and circuit breaker (reads DD_API_KEY, DD_SITE, DD_URL)
    let forwarder_config = DatadogConfiguration::from_configuration(&config)
        .error_context("Failed to create Datadog forwarder configuration")?;
    blueprint.add_forwarder("datadog_forwarder", forwarder_config)?;

    // --- Wiring ---
    blueprint.connect_component("aggregate", ["dogstatsd", "enhanced_metrics"])?;
    blueprint.connect_component("metrics_encoder", ["aggregate"])?;
    blueprint.connect_component("datadog_forwarder", ["metrics_encoder"])?;

    // --- Build and spawn on the current tokio runtime ---
    let built = blueprint
        .build()
        .await
        .error_context("Failed to build metrics topology")?;

    debug!("Metrics topology built, spawning on current runtime...");

    // Use noop memory limiter — Lambda has its own memory management
    let memory_limiter = MemoryLimiter::noop();

    let running = built
        .spawn_with_handle(
            &health_registry,
            memory_limiter,
            tokio::runtime::Handle::current(),
        )
        .await
        .error_context("Failed to spawn metrics topology")?;

    debug!("Saluki metrics topology started.");

    Ok(MetricsTopology {
        enhanced_metrics_handle,
        aggregator_handle,
        running,
    })
}
