use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use tracing::{debug, error};

use crate::LAMBDA_RUNTIME_SLUG;
use crate::config::{self, processing_rule};
use crate::event_bus::Event;
use crate::extension::telemetry::events::TelemetryEvent;
use crate::logs::aggregator_service::AggregatorHandle;
use crate::logs::lambda::processor::LambdaProcessor;
use crate::tags;

impl LogsProcessor {
    #[must_use]
    pub fn new(
        config: Arc<config::Config>,
        tags_provider: Arc<tags::provider::Provider>,
        event_bus: Sender<Event>,
        runtime: String,
        is_managed_instance_mode: bool,
    ) -> Self {
        match runtime.as_str() {
            LAMBDA_RUNTIME_SLUG => {
                let lambda_processor = LambdaProcessor::new(
                    tags_provider,
                    config,
                    event_bus,
                    is_managed_instance_mode,
                );
                LogsProcessor::Lambda(lambda_processor)
            }
            _ => panic!("Unsupported runtime: {runtime}"),
        }
    }

    // TODO: rename this method to process_telemetry_event()
    pub async fn process(&mut self, event: TelemetryEvent, aggregator_handle: &AggregatorHandle) {
        match self {
            LogsProcessor::Lambda(lambda_processor) => {
                lambda_processor.process(event, aggregator_handle).await;
            }
        }
    }

    pub fn process_durable_context_update(
        &mut self,
        (request_id, execution_id, execution_name): (String, String, String),
        aggregator_handle: &AggregatorHandle,
    ) {
        self.insert_to_durable_map(&request_id, &execution_id, &execution_name);
        let ready_logs = self.take_ready_logs();
        if !ready_logs.is_empty() && let Err(e) = aggregator_handle.insert_batch(ready_logs) {
            error!("LOGS_PROCESSOR | Failed to insert batch: {}", e);
        }
    }

    pub fn insert_to_durable_map(
        &mut self,
        request_id: &str,
        execution_id: &str,
        execution_name: &str,
    ) {
        match self {
            LogsProcessor::Lambda(p) => {
                p.insert_to_durable_context_map(request_id, execution_id, execution_name);
            }
        }
    }

    pub fn take_ready_logs(&mut self) -> Vec<String> {
        match self {
            LogsProcessor::Lambda(p) => p.take_ready_logs(),
        }
    }
}

#[allow(clippy::module_name_repetitions)]
#[derive(Clone, Debug)]
pub enum LogsProcessor {
    Lambda(LambdaProcessor),
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub kind: processing_rule::Kind,
    pub regex: regex::Regex,
    pub placeholder: String,
}

pub trait Processor<L> {
    fn apply_rules(rules: &Option<Vec<Rule>>, message: &mut String) -> bool {
        match &rules {
            // No need to apply if there are no rules
            None => true,
            Some(rules) => {
                // If rules are empty, we don't need to apply them
                if rules.is_empty() {
                    return true;
                }

                // Process rules
                for rule in rules {
                    match rule.kind {
                        processing_rule::Kind::ExcludeAtMatch => {
                            if rule.regex.is_match(message) {
                                return false;
                            }
                        }
                        processing_rule::Kind::IncludeAtMatch => {
                            if !rule.regex.is_match(message) {
                                return false;
                            }
                        }
                        processing_rule::Kind::MaskSequences => {
                            *message = rule
                                .regex
                                .replace_all(message, rule.placeholder.as_str())
                                .to_string();
                        }
                    }
                }
                true
            }
        }
    }

    fn compile_rules(
        rules: &Option<Vec<config::processing_rule::ProcessingRule>>,
    ) -> Option<Vec<Rule>> {
        match rules {
            None => None,
            Some(rules) => {
                if rules.is_empty() {
                    return None;
                }
                let mut compiled_rules = Vec::new();

                for rule in rules {
                    match regex::Regex::new(&rule.pattern) {
                        Ok(regex) => {
                            let placeholder = rule.replace_placeholder.clone().unwrap_or_default();
                            compiled_rules.push(Rule {
                                kind: rule.kind,
                                regex,
                                placeholder,
                            });
                        }
                        Err(e) => {
                            debug!("Failed to compile rule: {}", e);
                        }
                    }
                }

                Some(compiled_rules)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct TestProcessor;
    impl Processor<String> for TestProcessor {}

    #[test]
    fn test_apply_rules_mask_sequences() {
        let rules = vec![Rule {
            kind: processing_rule::Kind::MaskSequences,
            regex: regex::Regex::new("replace-me").unwrap(),
            placeholder: "test-placeholder".to_string(),
        }];
        let mut message = "do-not-replace replace-me".to_string();

        let should_include = TestProcessor::apply_rules(&Some(rules), &mut message);
        assert!(should_include);
        assert_eq!(message, "do-not-replace test-placeholder");
    }

    #[test]
    fn test_apply_rules_exclude_at_match() {
        let rules = vec![Rule {
            kind: processing_rule::Kind::ExcludeAtMatch,
            regex: regex::Regex::new("exclude-me").unwrap(),
            placeholder: "test-placeholder".to_string(),
        }];
        let mut message = "exclude-me".to_string();

        let should_include = TestProcessor::apply_rules(&Some(rules), &mut message);
        assert!(!should_include);
    }

    #[test]
    fn test_apply_rules_include_at_match() {
        let rules = Some(vec![Rule {
            kind: processing_rule::Kind::IncludeAtMatch,
            regex: regex::Regex::new("include-me").unwrap(),
            placeholder: "test-placeholder".to_string(),
        }]);

        let mut message = "include-me".to_string();
        let should_include = TestProcessor::apply_rules(&rules, &mut message);
        assert!(should_include);

        let mut message = "do-not-include-me".to_string();
        let should_include = TestProcessor::apply_rules(&rules, &mut message);
        assert!(should_include);
    }

    #[test]
    fn test_compile_rules() {
        let rules = vec![processing_rule::ProcessingRule {
            kind: processing_rule::Kind::MaskSequences,
            name: "test".to_string(),
            pattern: "test-pattern".to_string(),
            replace_placeholder: Some("test-placeholder".to_string()),
        }];

        let compiled_rules = TestProcessor::compile_rules(&Some(rules));
        assert!(compiled_rules.is_some());
        assert_eq!(compiled_rules.unwrap().len(), 1);
    }

    #[test]
    fn test_compile_rules_empty() {
        let rules = vec![];

        let compiled_rules = TestProcessor::compile_rules(&Some(rules));
        assert!(compiled_rules.is_none());
    }

    #[test]
    fn test_compile_rules_none() {
        let compiled_rules = TestProcessor::compile_rules(&None);
        assert!(compiled_rules.is_none());
    }

    #[test]
    fn test_compile_rules_invalid_regex() {
        let rules = vec![processing_rule::ProcessingRule {
            kind: processing_rule::Kind::MaskSequences,
            name: "test".to_string(),
            pattern: "(".to_string(),
            replace_placeholder: Some("test-placeholder".to_string()),
        }];

        let compiled_rules = TestProcessor::compile_rules(&Some(rules));
        assert!(compiled_rules.is_some());
        assert_eq!(compiled_rules.unwrap().len(), 0);
    }
}
