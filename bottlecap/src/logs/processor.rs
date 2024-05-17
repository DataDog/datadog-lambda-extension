use tracing::debug;

use crate::config;
use crate::config::processing_rule;

#[derive(Clone, Debug)]
pub struct Rule {
    pub kind: processing_rule::Kind,
    pub regex: regex::Regex,
    pub placeholder: String,
}

pub trait Processor<L> {
    fn apply_rules(&self, message: &mut L) -> bool;
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
mod tests {
    use super::*;

    struct TestProcessor;
    impl Processor<String> for TestProcessor {
        fn apply_rules(&self, _: &mut String) -> bool {
            true
        }
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
