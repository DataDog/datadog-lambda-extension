use std::env;
use std::sync::Arc;

use tokio::sync::{Mutex, OnceCell};
use tracing::error;

use crate::appsec::processor::{Error as AppSecError, Processor};
use crate::config::Config;

pub mod processor;

/// A [`Processor`] shared across the trace agent and the runtime API proxy.
///
/// Wrapped in a [`Mutex`] (and an [`Arc`] so the value stays [`Send`]) because
/// the WAF context buffer is mutated from multiple asynchronous tasks.
pub type SharedProcessor = Arc<Mutex<Processor>>;

/// A handle to a [`SharedProcessor`] whose construction is deferred off the
/// initialization critical path.
///
/// Building the WAF (zstd-decompressing and JSON-parsing the ruleset, then
/// compiling it through `libddwaf`) costs tens of milliseconds and is only
/// needed once the first request payload is evaluated — which is strictly after
/// the first `/next`. Rather than block init on that work, consumers hold this
/// awaitable handle and resolve it (via [`resolve`]) at the point where they
/// actually need the WAF.
///
/// The outer [`Option`] (see [`defer_processor`]) distinguishes the
/// feature-disabled case (no handle at all) from the enabled case. The inner
/// [`Option`] distinguishes a successfully built processor (`Some`) from a build
/// that failed (`None`), in which case the feature is treated as a no-op exactly
/// as it would have been with the previous eager construction.
pub type DeferredProcessor = Arc<OnceCell<Option<SharedProcessor>>>;

/// Determines whether the Serverless App & API Protection features are enabled.
#[must_use]
pub const fn is_enabled(cfg: &Config) -> bool {
    cfg.ext.serverless_appsec_enabled
}

/// Determines whether APM is only used as a transport for App & API Protection,
/// instead of being used for tracing as well.
#[must_use]
pub fn is_standalone() -> bool {
    env::var("DD_APM_TRACING_ENABLED").is_ok_and(|s| s.to_lowercase() == "true")
}

/// Prepares a [`DeferredProcessor`] for the App & API Protection feature without
/// blocking the caller on the (CPU-bound) WAF build.
///
/// Returns [`None`] immediately when the feature is disabled, preserving the
/// cheap, synchronous disabled-by-default path. When enabled, a background task
/// is spawned to build the processor off the critical path, and a handle that
/// resolves to it is returned right away. Consumers call [`resolve`] where they
/// use the WAF; if a request arrives before the background build has finished,
/// that call simply awaits the in-flight build (or kicks it off itself).
#[must_use]
pub fn defer_processor(cfg: &Arc<Config>) -> Option<DeferredProcessor> {
    if !is_enabled(cfg) {
        // Feature disabled: nothing to build, and nothing to await.
        return None;
    }

    let cell: DeferredProcessor = Arc::new(OnceCell::new());

    // Kick the build off the synchronous init path. The first consumer to call
    // `resolve` will await whatever this task produces (or, if it somehow races
    // ahead, run an equivalent build itself via `get_or_init`).
    let background_cell = Arc::clone(&cell);
    let background_cfg = Arc::clone(cfg);
    tokio::spawn(async move {
        let _ = background_cell
            .get_or_init(|| build_processor(background_cfg))
            .await;
    });

    Some(cell)
}

/// Resolves a [`DeferredProcessor`] to the underlying [`SharedProcessor`], if the
/// WAF was built successfully.
///
/// Awaits the background build started by [`defer_processor`] when it is still in
/// flight; if no build is running yet, it starts one (off-thread, CPU-bound work
/// runs on the blocking pool). Subsequent calls return immediately.
pub async fn resolve(handle: &DeferredProcessor, cfg: &Arc<Config>) -> Option<SharedProcessor> {
    handle
        .get_or_init(|| build_processor(Arc::clone(cfg)))
        .await
        .clone()
}

/// Builds the App & API Protection [`Processor`] on the blocking thread pool.
///
/// The WAF build is CPU-bound (ruleset decompression, JSON parsing, and WAF
/// compilation), so it is offloaded with [`tokio::task::spawn_blocking`] to keep
/// it off the async worker threads. Returns [`None`] (logging at the appropriate
/// level) when the feature is disabled or the build fails, matching the previous
/// "feature is silently a no-op" behaviour.
async fn build_processor(cfg: Arc<Config>) -> Option<SharedProcessor> {
    match tokio::task::spawn_blocking(move || Processor::new(&cfg)).await {
        Ok(Ok(processor)) => Some(Arc::new(Mutex::new(processor))),
        Ok(Err(AppSecError::FeatureDisabled)) => None,
        Ok(Err(e)) => {
            error!(
                "AAP | error creating App & API Protection processor, the feature will be disabled: {e}"
            );
            None
        }
        Err(e) => {
            error!("AAP | App & API Protection processor build task failed to join: {e}");
            None
        }
    }
}
