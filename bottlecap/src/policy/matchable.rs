//! Matchable trait implementations for bottlecap telemetry types.
//!
//! This module provides implementations of the `policy_rs::Matchable` trait
//! for `IntakeLog` (logs) and `SpanWrapper` (traces), enabling policy evaluation
//! against these telemetry types.

use policy_rs::proto::tero::policy::v1::LogField;
use policy_rs::{LogFieldSelector, Matchable};

use crate::logs::lambda::IntakeLog;
use libdd_trace_protobuf::pb;

impl Matchable for IntakeLog {
    fn get_field(&self, field: &LogFieldSelector) -> Option<&str> {
        match field {
            LogFieldSelector::Simple(simple) => match simple {
                LogField::Body => Some(&self.message.message),
                LogField::SeverityText => Some(&self.message.status),
                _ => None,
            },
            LogFieldSelector::ResourceAttribute(key) => match key.as_str() {
                "service" => Some(&self.service),
                "host" | "hostname" => Some(&self.hostname),
                "source" => Some(&self.source),
                "arn" => Some(&self.message.lambda.arn),
                "request_id" => self.message.lambda.request_id.as_deref(),
                _ => None,
            },
            // Note: LogAttribute matching against tags is not supported because
            // tags are stored as a comma-separated string and we cannot return
            // a &str reference to a parsed value. Policies should use
            // ResourceAttribute for known fields instead.
            LogFieldSelector::LogAttribute(_) => None,
            LogFieldSelector::ScopeAttribute(_) => None,
        }
    }
}

/// Wrapper around `pb::Span` to implement `Matchable` trait.
///
/// Due to Rust's orphan rules, we cannot implement a foreign trait (`Matchable`)
/// for a foreign type (`pb::Span`). This newtype wrapper allows us to implement
/// the trait while maintaining zero-cost abstraction.
pub struct SpanWrapper<'a>(pub &'a pb::Span);

impl Matchable for SpanWrapper<'_> {
    fn get_field(&self, field: &LogFieldSelector) -> Option<&str> {
        match field {
            LogFieldSelector::Simple(simple) => match simple {
                LogField::Body => Some(&self.0.resource),
                _ => None,
            },
            LogFieldSelector::ResourceAttribute(key) => match key.as_str() {
                "service" => Some(&self.0.service),
                "name" => Some(&self.0.name),
                "resource" => Some(&self.0.resource),
                "type" => Some(&self.0.r#type),
                _ => self.0.meta.get(key).map(String::as_str),
            },
            LogFieldSelector::LogAttribute(key) => self.0.meta.get(key).map(String::as_str),
            LogFieldSelector::ScopeAttribute(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::lambda::{Lambda, Message};
    use std::collections::HashMap;

    fn create_test_intake_log() -> IntakeLog {
        IntakeLog {
            message: Message {
                message: "Test log message".to_string(),
                lambda: Lambda {
                    arn: "arn:aws:lambda:us-east-1:123456789:function:test".to_string(),
                    request_id: Some("req-123".to_string()),
                },
                timestamp: 1234567890,
                status: "info".to_string(),
            },
            hostname: "test-host".to_string(),
            service: "test-service".to_string(),
            tags: "env:test,region:us-east-1".to_string(),
            source: "lambda".to_string(),
        }
    }

    fn create_test_span() -> pb::Span {
        let mut meta = HashMap::new();
        meta.insert("env".to_string(), "production".to_string());
        meta.insert("custom_tag".to_string(), "custom_value".to_string());

        pb::Span {
            name: "test.span".to_string(),
            service: "test-service".to_string(),
            resource: "/api/users".to_string(),
            r#type: "web".to_string(),
            meta,
            ..Default::default()
        }
    }

    #[test]
    fn test_intake_log_body() {
        let log = create_test_intake_log();
        assert_eq!(
            log.get_field(&LogFieldSelector::Simple(LogField::Body)),
            Some("Test log message")
        );
    }

    #[test]
    fn test_intake_log_severity() {
        let log = create_test_intake_log();
        assert_eq!(
            log.get_field(&LogFieldSelector::Simple(LogField::SeverityText)),
            Some("info")
        );
    }

    #[test]
    fn test_intake_log_resource_attributes() {
        let log = create_test_intake_log();

        assert_eq!(
            log.get_field(&LogFieldSelector::ResourceAttribute("service".to_string())),
            Some("test-service")
        );
        assert_eq!(
            log.get_field(&LogFieldSelector::ResourceAttribute("hostname".to_string())),
            Some("test-host")
        );
        assert_eq!(
            log.get_field(&LogFieldSelector::ResourceAttribute("arn".to_string())),
            Some("arn:aws:lambda:us-east-1:123456789:function:test")
        );
        assert_eq!(
            log.get_field(&LogFieldSelector::ResourceAttribute(
                "request_id".to_string()
            )),
            Some("req-123")
        );
    }

    #[test]
    fn test_intake_log_missing_request_id() {
        let mut log = create_test_intake_log();
        log.message.lambda.request_id = None;

        assert_eq!(
            log.get_field(&LogFieldSelector::ResourceAttribute(
                "request_id".to_string()
            )),
            None
        );
    }

    #[test]
    fn test_span_body() {
        let span = create_test_span();
        let wrapper = SpanWrapper(&span);
        assert_eq!(
            wrapper.get_field(&LogFieldSelector::Simple(LogField::Body)),
            Some("/api/users")
        );
    }

    #[test]
    fn test_span_resource_attributes() {
        let span = create_test_span();
        let wrapper = SpanWrapper(&span);

        assert_eq!(
            wrapper.get_field(&LogFieldSelector::ResourceAttribute("service".to_string())),
            Some("test-service")
        );
        assert_eq!(
            wrapper.get_field(&LogFieldSelector::ResourceAttribute("name".to_string())),
            Some("test.span")
        );
        assert_eq!(
            wrapper.get_field(&LogFieldSelector::ResourceAttribute("type".to_string())),
            Some("web")
        );
    }

    #[test]
    fn test_span_meta_via_resource_attribute() {
        let span = create_test_span();
        let wrapper = SpanWrapper(&span);

        assert_eq!(
            wrapper.get_field(&LogFieldSelector::ResourceAttribute("env".to_string())),
            Some("production")
        );
    }

    #[test]
    fn test_span_log_attribute() {
        let span = create_test_span();
        let wrapper = SpanWrapper(&span);

        assert_eq!(
            wrapper.get_field(&LogFieldSelector::LogAttribute("custom_tag".to_string())),
            Some("custom_value")
        );
        assert_eq!(
            wrapper.get_field(&LogFieldSelector::LogAttribute("nonexistent".to_string())),
            None
        );
    }
}
