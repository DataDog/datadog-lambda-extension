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
    fn apply_rules(&self, log: &mut L) -> bool;
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
