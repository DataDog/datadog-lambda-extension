//! Policy evaluator wrapper for simplified policy evaluation.
//!
//! This module provides a `PolicyEvaluator` struct that wraps the `PolicyEngine`
//! and `PolicyRegistry` from `policy-rs`, providing a simple `should_keep` method
//! for filtering telemetry.

use policy_rs::{EvaluateResult, Matchable, PolicyEngine, PolicyRegistry};
use std::sync::Arc;
use tracing::{debug, warn};

/// Evaluates telemetry items against configured policies.
///
/// The evaluator holds a reference to the policy registry and creates
/// snapshots for lock-free evaluation. It provides a simple boolean
/// interface for determining whether telemetry should be kept or dropped.
pub struct PolicyEvaluator {
    engine: PolicyEngine,
    registry: Arc<PolicyRegistry>,
}

impl PolicyEvaluator {
    /// Creates a new policy evaluator with the given registry.
    #[must_use]
    pub fn new(registry: Arc<PolicyRegistry>) -> Self {
        Self {
            engine: PolicyEngine::new(),
            registry,
        }
    }

    /// Evaluates an item against configured policies.
    ///
    /// Returns `true` if the item should be kept (sent to backend),
    /// `false` if it should be dropped.
    ///
    /// This method implements a "fail open" policy: if evaluation fails
    /// for any reason, the item is kept and a warning is logged.
    ///
    /// # Policy Results
    /// - `NoMatch`: Keep (no policy matched)
    /// - `Keep`: Keep
    /// - `Drop`: Drop
    /// - `Sample`: Keep based on sampling decision
    /// - `RateLimit`: Keep based on rate limit decision
    pub async fn should_keep<M: Matchable>(&self, item: &M) -> bool {
        let snapshot = self.registry.snapshot();

        match self.engine.evaluate(&snapshot, item).await {
            Ok(result) => match result {
                EvaluateResult::NoMatch => true,
                EvaluateResult::Keep { .. } => true,
                EvaluateResult::Drop { .. } => {
                    debug!("POLICY | Dropping item due to policy");
                    false
                }
                EvaluateResult::Sample {
                    keep, percentage, ..
                } => {
                    if !keep {
                        debug!("POLICY | Dropping item due to sampling ({}%)", percentage);
                    }
                    keep
                }
                EvaluateResult::RateLimit { allowed, .. } => {
                    if !allowed {
                        debug!("POLICY | Dropping item due to rate limit");
                    }
                    allowed
                }
            },
            Err(e) => {
                // Fail open: keep the item if evaluation fails
                warn!("POLICY | Evaluation error, keeping item: {}", e);
                true
            }
        }
    }

    /// Evaluates an item synchronously by blocking on the async evaluation.
    ///
    /// This is a convenience method for contexts where async is not available.
    /// It uses `futures::executor::block_on` internally.
    ///
    /// Returns `true` if the item should be kept, `false` if it should be dropped.
    #[must_use]
    pub fn should_keep_sync<M: Matchable>(&self, item: &M) -> bool {
        let snapshot = self.registry.snapshot();

        match futures::executor::block_on(self.engine.evaluate(&snapshot, item)) {
            Ok(result) => match result {
                EvaluateResult::NoMatch => true,
                EvaluateResult::Keep { .. } => true,
                EvaluateResult::Drop { .. } => {
                    debug!("POLICY | Dropping item due to policy");
                    false
                }
                EvaluateResult::Sample {
                    keep, percentage, ..
                } => {
                    if !keep {
                        debug!("POLICY | Dropping item due to sampling ({}%)", percentage);
                    }
                    keep
                }
                EvaluateResult::RateLimit { allowed, .. } => {
                    if !allowed {
                        debug!("POLICY | Dropping item due to rate limit");
                    }
                    allowed
                }
            },
            Err(e) => {
                // Fail open: keep the item if evaluation fails
                warn!("POLICY | Evaluation error, keeping item: {}", e);
                true
            }
        }
    }

    /// Returns a reference to the underlying registry.
    #[must_use]
    pub fn registry(&self) -> &Arc<PolicyRegistry> {
        &self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use policy_rs::LogFieldSelector;

    struct TestItem {
        body: String,
    }

    impl Matchable for TestItem {
        fn get_field(&self, field: &LogFieldSelector) -> Option<&str> {
            match field {
                LogFieldSelector::Simple(policy_rs::proto::tero::policy::v1::LogField::Body) => {
                    Some(&self.body)
                }
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn test_evaluator_no_policies() {
        let registry = Arc::new(PolicyRegistry::new());
        let evaluator = PolicyEvaluator::new(registry);

        let item = TestItem {
            body: "test message".to_string(),
        };

        // With no policies, everything should be kept
        assert!(evaluator.should_keep(&item).await);
    }

    #[test]
    fn test_evaluator_sync_no_policies() {
        let registry = Arc::new(PolicyRegistry::new());
        let evaluator = PolicyEvaluator::new(registry);

        let item = TestItem {
            body: "test message".to_string(),
        };

        // With no policies, everything should be kept
        assert!(evaluator.should_keep_sync(&item));
    }
}
