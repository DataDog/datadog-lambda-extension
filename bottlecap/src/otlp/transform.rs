use hex;
use lazy_static::lazy_static;
use libdd_trace_normalization::normalize_utils::{
    normalize_name, normalize_service, normalize_tag,
};
use libdd_trace_protobuf::pb::Span as DatadogSpan;
use opentelemetry_proto::tonic::common::v1::{
    AnyValue, InstrumentationScope as OtelInstrumentationScope, KeyValue, any_value,
};
use opentelemetry_proto::tonic::resource::v1::Resource as OtelResource;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use opentelemetry_proto::tonic::trace::v1::{
    Span as OtelSpan,
    span::{Event, Link, SpanKind},
    status::StatusCode,
};
use opentelemetry_semantic_conventions::attribute::{
    DB_COLLECTION_NAME, DB_NAMESPACE, DB_OPERATION_NAME, DB_QUERY_SUMMARY, DB_QUERY_TEXT,
    DB_STORED_PROCEDURE_NAME, DB_SYSTEM_NAME, DEPLOYMENT_ENVIRONMENT_NAME, FAAS_INVOKED_NAME,
    FAAS_INVOKED_PROVIDER, FAAS_TRIGGER, GRAPHQL_OPERATION_NAME, GRAPHQL_OPERATION_TYPE,
    HTTP_RESPONSE_STATUS_CODE, HTTP_ROUTE, MESSAGING_DESTINATION_NAME, MESSAGING_OPERATION_TYPE,
    MESSAGING_SYSTEM, OTEL_SCOPE_NAME, OTEL_SCOPE_VERSION, OTEL_STATUS_CODE,
    OTEL_STATUS_DESCRIPTION, RPC_METHOD, RPC_SERVICE, RPC_SYSTEM, SERVER_ADDRESS, SERVER_PORT,
};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION}; // CONTAINER_ID, SERVICE_VERSION, TELEMETRY_SDK_LANGUAGE, TELEMETRY_SDK_VERSION,
use opentelemetry_semantic_conventions::trace::{
    EXCEPTION_MESSAGE, EXCEPTION_STACKTRACE, EXCEPTION_TYPE,
};
use serde_json::json;
use std::collections::HashMap;
use std::str;
use std::sync::Arc;
use tracing::debug;

use crate::config::Config;
use crate::traces::propagation::text_map_propagator::DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY;

// Constants for Datadog namespace keys
pub const KEY_DATADOG_SERVICE: &str = "datadog.service";
pub const KEY_DATADOG_NAME: &str = "datadog.name";
pub const KEY_DATADOG_RESOURCE: &str = "datadog.resource";
pub const KEY_DATADOG_TYPE: &str = "datadog.type";
pub const KEY_DATADOG_SPAN_KIND: &str = "datadog.span.kind";
pub const KEY_DATADOG_ERROR: &str = "datadog.error";
pub const KEY_DATADOG_ERROR_MSG: &str = "datadog.error.msg";
pub const KEY_DATADOG_ERROR_TYPE: &str = "datadog.error.type";
pub const KEY_DATADOG_ERROR_STACK: &str = "datadog.error.stack";
pub const KEY_DATADOG_VERSION: &str = "datadog.version";
pub const KEY_DATADOG_HTTP_STATUS_CODE: &str = "datadog.http_status_code";
pub const KEY_DATADOG_HOST: &str = "datadog.host";
pub const KEY_DATADOG_ENVIRONMENT: &str = "datadog.env";
pub const KEY_DATADOG_CONTAINER_ID: &str = "datadog.container_id";
pub const KEY_DATADOG_CONTAINER_TAGS: &str = "datadog.container_tags";

// const MAX_RESOURCE_LENGTH: usize = 5_000;
const KEY_SAMPLING_RATE_EVENT_EXTRACTION: &str = "_dd1.sr.eausr";
const KEY_SAMPLING_PRIORITY_V1: &str = "_sampling_priority_v1";
pub const KEY_DATADOG_STATS_COMPUTED: &str = "_dd.stats_computed";

// const SPAN_TYPE_GENERIC_DB: &str = "db";

// AWS Lambda OTEL instrumentation scope name
const AWS_LAMBDA_INSTRUMENTATION_SCOPE: &str = "opentelemetry.instrumentation.aws_lambda";

lazy_static! {
    // TODO: add mappings
    static ref DB_SYSTEM_MAP: HashMap<String, String> = HashMap::new();
    static ref DD_SEMANTIC_KEYS_TO_META_KEYS: HashMap<&'static str, &'static str> = HashMap::from([
        (KEY_DATADOG_ENVIRONMENT, "env"),
        (KEY_DATADOG_VERSION, "version"),
        (KEY_DATADOG_HTTP_STATUS_CODE, "http.status_code"),
        (KEY_DATADOG_ERROR_MSG, "error.msg"),
        (KEY_DATADOG_ERROR_TYPE, "error.type"),
        (KEY_DATADOG_ERROR_STACK, "error.stack"),
    ]);

    static ref META_KEYS_TO_DD_SEMANTIC_KEYS: HashMap<&'static str, &'static str> = HashMap::from([
        ("env", KEY_DATADOG_ENVIRONMENT),
        ("version", KEY_DATADOG_VERSION),
        ("http.status_code", KEY_DATADOG_HTTP_STATUS_CODE),
        ("error.msg", KEY_DATADOG_ERROR_MSG),
        ("error.type", KEY_DATADOG_ERROR_TYPE),
        ("error.stack", KEY_DATADOG_ERROR_STACK),
    ]);
}

fn get_otel_attribute_value(attributes: &Vec<KeyValue>, key: &str) -> Option<AnyValue> {
    for attribute in attributes {
        if attribute.key == key {
            return attribute.value.clone();
        }
    }

    None
}

fn otel_value_to_string(value: &any_value::Value) -> String {
    match value {
        any_value::Value::StringValue(s) => s.clone(),
        any_value::Value::BoolValue(b) => b.to_string(),
        any_value::Value::IntValue(i) => i.to_string(),
        any_value::Value::DoubleValue(d) => d.to_string(),
        any_value::Value::BytesValue(b) => hex::encode(b),
        any_value::Value::ArrayValue(a) => {
            let array: Vec<String> = a
                .values
                .iter()
                .filter_map(|v| v.value.as_ref().map(otel_value_to_string))
                .collect();
            serde_json::to_string(&array).unwrap_or_default()
        }
        any_value::Value::KvlistValue(kvl) => {
            let hashmap: HashMap<String, String> = kvl
                .values
                .iter()
                .filter_map(|kv| {
                    let key = kv.key.clone();
                    let value = kv
                        .value
                        .as_ref()
                        .and_then(|v| v.value.as_ref())
                        .map(otel_value_to_string);

                    value.map(|v| (key, v))
                })
                .collect();
            serde_json::to_string(&hashmap).unwrap_or_default()
        }
    }
}

/// Returns the matched value as a string from the attributes if found.
///
/// The first value found will be returned.
///
/// If normalize is true, the value will be normalized.
#[must_use]
pub fn get_otel_attribute_value_as_string(
    attributes: &Vec<KeyValue>,
    key: &str,
    normalize: bool,
) -> String {
    let mut value = String::new();
    for attribute in attributes {
        if attribute.key == key {
            let Some(any_value) = &attribute.value else {
                continue;
            };

            let Some(v) = &any_value.value else {
                continue;
            };

            value = otel_value_to_string(v);

            break;
        }
    }

    if normalize {
        normalize_tag(&mut value);
    }

    value
}

#[must_use]
pub fn get_otel_attribute_value_as_string_from_resource_or_span(
    otel_res: &OtelResource,
    otel_span: &OtelSpan,
    key: &str,
    normalize: bool,
) -> String {
    let value = get_otel_attribute_value_as_string(&otel_res.attributes, key, normalize);
    if !value.is_empty() {
        return value;
    }

    get_otel_attribute_value_as_string(&otel_span.attributes, key, normalize)
}

#[must_use]
pub fn otel_trace_id_to_dd_id(trace_id: &[u8]) -> (u64, u64) {
    let lower_order_bits = u64::from_be_bytes(trace_id[8..16].try_into().unwrap_or_default());
    let higher_order_bits = u64::from_be_bytes(trace_id[0..8].try_into().unwrap_or_default());

    (lower_order_bits, higher_order_bits)
}

#[must_use]
pub fn otel_span_id_to_u64(span_id: &[u8]) -> u64 {
    u64::from_be_bytes(span_id.try_into().unwrap_or_default())
}

// Checks if the new operation and resource name logic should be used
fn otel_operation_and_resource_v2_enabled(config: Arc<Config>) -> bool {
    !config.otlp_config_traces_span_name_as_resource_name
        && config.otlp_config_traces_span_name_remappings.is_empty()
        && !config
            .apm_features
            .contains(&"disable_operation_and_resource_name_logic_v2".to_string())
}

fn get_otel_service(otel_res: &OtelResource, normalize: bool) -> String {
    let mut service =
        get_otel_attribute_value_as_string(&otel_res.attributes, SERVICE_NAME, normalize);

    if service.is_empty() {
        service = String::from("otlpresourcenoservicename");
    }

    if normalize {
        normalize_service(&mut service);
    }

    service
}

fn get_otel_operation_name_v1(
    otel_span: &OtelSpan,
    lib: &OtelInstrumentationScope,
    span_name_as_resource_name: bool,
    span_name_remappings: &HashMap<String, String>,
    normalize: bool,
) -> String {
    let mut operation_name =
        get_otel_attribute_value_as_string(&otel_span.attributes, "operation.name", false);
    if operation_name.is_empty() {
        if span_name_as_resource_name {
            operation_name.clone_from(&otel_span.name);
        } else {
            let span_kind_name = get_dd_span_kind_from_otel_kind(otel_span);
            if lib.name.is_empty() {
                operation_name = format!("opentelemetry.{span_kind_name}");
            } else {
                operation_name = format!("{}.{}", lib.name, span_kind_name);
            }
        }
    }

    if let Some(remapping_name) = span_name_remappings.get(&operation_name) {
        operation_name = remapping_name.clone();
    }

    if normalize {
        normalize_name(&mut operation_name);
    }

    operation_name
}

#[allow(clippy::too_many_lines)]
fn get_otel_operation_name_v2(otel_span: &OtelSpan, lib: &OtelInstrumentationScope) -> String {
    let operation_name =
        get_otel_attribute_value_as_string(&otel_span.attributes, "operation.name", false);
    if !operation_name.is_empty() {
        return operation_name;
    }

    let is_client = otel_span.kind() == SpanKind::Client;
    let is_server = otel_span.kind() == SpanKind::Server;

    // AWS Lambda: Check if this is the root Lambda invocation span
    // Only applies to Server spans from the AWS Lambda OTEL instrumentation
    if is_server && lib.name == AWS_LAMBDA_INSTRUMENTATION_SCOPE {
        return "aws.lambda".to_string();
    }

    // HTTP
    let method =
        get_otel_attribute_value_as_string(&otel_span.attributes, "http.request.method", false);
    if !method.is_empty() {
        if is_server {
            return "http.server.request".to_string();
        }
        if is_client {
            return "http.client.request".to_string();
        }
    }

    // Database
    let db_summary =
        get_otel_attribute_value_as_string(&otel_span.attributes, DB_QUERY_SUMMARY, true);
    if !db_summary.is_empty() {
        return db_summary;
    }

    let get = |k| {
        let v = get_otel_attribute_value_as_string(&otel_span.attributes, k, true);
        (!v.is_empty()).then_some(v)
    };
    let target = get(DB_COLLECTION_NAME)
        .or_else(|| get(DB_STORED_PROCEDURE_NAME))
        .or_else(|| get(DB_NAMESPACE))
        .or_else(|| {
            let addr = get(SERVER_ADDRESS)?;
            let port = get(SERVER_PORT)?;
            Some(format!("{addr}:{port}"))
        })
        .unwrap_or_default();

    let db_operation =
        get_otel_attribute_value_as_string(&otel_span.attributes, DB_OPERATION_NAME, true);
    if !target.is_empty() {
        if !db_operation.is_empty() {
            return format!("{db_operation} {target}");
        }
        return target;
    }

    let db_system = get_otel_attribute_value_as_string(&otel_span.attributes, DB_SYSTEM_NAME, true);
    if !db_system.is_empty() {
        return db_system;
    }

    // Messaging
    let messaging_system =
        get_otel_attribute_value_as_string(&otel_span.attributes, MESSAGING_SYSTEM, true);
    let messaging_operation =
        get_otel_attribute_value_as_string(&otel_span.attributes, MESSAGING_OPERATION_TYPE, true);
    if !messaging_system.is_empty() && !messaging_operation.is_empty() {
        match otel_span.kind() {
            SpanKind::Client | SpanKind::Server | SpanKind::Consumer | SpanKind::Producer => {
                return format!("{messaging_system}.{messaging_operation}");
            }
            _ => {}
        }
    }

    // RPC & AWS
    let rpc_system = get_otel_attribute_value_as_string(&otel_span.attributes, RPC_SYSTEM, true);
    let is_rpc = !rpc_system.is_empty();
    let is_aws = is_rpc && rpc_system == "aws-api";

    // AWS client
    if is_aws && is_client {
        let service = get_otel_attribute_value_as_string(&otel_span.attributes, RPC_SERVICE, true);
        if !service.is_empty() {
            return format!("aws.{service}.request");
        }
        return "aws.client.request".to_string();
    }

    // RPC client
    if is_rpc && is_client {
        return format!("{rpc_system}.client.request");
    }

    // RPC server
    if is_rpc && is_server {
        return format!("{rpc_system}.server.request");
    }

    // FaaS client
    let provider =
        get_otel_attribute_value_as_string(&otel_span.attributes, FAAS_INVOKED_PROVIDER, true);
    let invoked_name =
        get_otel_attribute_value_as_string(&otel_span.attributes, FAAS_INVOKED_NAME, true);
    if !provider.is_empty() && !invoked_name.is_empty() && is_client {
        return format!("{provider}.{invoked_name}.invoke");
    }

    // FaaS server
    let trigger = get_otel_attribute_value_as_string(&otel_span.attributes, FAAS_TRIGGER, true);
    if !trigger.is_empty() && is_server {
        return format!("{trigger}.invoke");
    }

    // GraphQL
    if !get_otel_attribute_value_as_string(&otel_span.attributes, "graphql.operation.type", true)
        .is_empty()
    {
        return "graphql.server.request".to_string();
    }

    // If nothing matches, checking for generic http server/client
    let protocol =
        get_otel_attribute_value_as_string(&otel_span.attributes, "network.protocol.name", true);
    if is_server {
        if !protocol.is_empty() {
            return format!("{protocol}.server.request");
        }
        return "server.request".to_string();
    } else if is_client {
        if !protocol.is_empty() {
            return format!("{protocol}.client.request");
        }
        return "client.request".to_string();
    }

    if otel_span.kind() != SpanKind::Unspecified {
        return otel_span.kind().as_str_name().to_string();
    }

    SpanKind::Internal.as_str_name().to_string()
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn get_otel_resource(otel_span: &OtelSpan, otel_res: &OtelResource) -> String {
    let resource_name = get_otel_attribute_value_as_string_from_resource_or_span(
        otel_res,
        otel_span,
        "resource.name",
        false,
    );
    if !resource_name.is_empty() {
        return resource_name;
    }

    // HTTP
    let mut method = get_otel_attribute_value_as_string_from_resource_or_span(
        otel_res,
        otel_span,
        "http.request.method",
        false,
    );
    if !method.is_empty() {
        if method == "_OTHER" {
            method = String::from("HTTP");
        }

        // Use method and route if available
        if otel_span.kind() == SpanKind::Server {
            let route = get_otel_attribute_value_as_string_from_resource_or_span(
                otel_res, otel_span, HTTP_ROUTE, false,
            );
            if !route.is_empty() {
                return format!("{method} {route}");
            }
        }

        return method;
    }

    // Messaging
    let messaging_operation = get_otel_attribute_value_as_string_from_resource_or_span(
        otel_res,
        otel_span,
        MESSAGING_OPERATION_TYPE,
        false,
    );
    if !messaging_operation.is_empty() {
        let destination = get_otel_attribute_value_as_string_from_resource_or_span(
            otel_res,
            otel_span,
            MESSAGING_DESTINATION_NAME,
            false,
        );
        if !destination.is_empty() {
            return format!("{messaging_operation} {destination}");
        }

        return messaging_operation;
    }

    // RPC
    let rpc_method = get_otel_attribute_value_as_string_from_resource_or_span(
        otel_res, otel_span, RPC_METHOD, false,
    );
    if !rpc_method.is_empty() {
        let rpc_service = get_otel_attribute_value_as_string_from_resource_or_span(
            otel_res,
            otel_span,
            RPC_SERVICE,
            false,
        );
        if !rpc_service.is_empty() {
            return format!("{rpc_service} {rpc_method}");
        }

        return rpc_method;
    }

    // GraphQL
    let graphql_operation_type = get_otel_attribute_value_as_string_from_resource_or_span(
        otel_res,
        otel_span,
        GRAPHQL_OPERATION_TYPE,
        false,
    );
    if !graphql_operation_type.is_empty() {
        let graphql_operation_name = get_otel_attribute_value_as_string_from_resource_or_span(
            otel_res,
            otel_span,
            GRAPHQL_OPERATION_NAME,
            false,
        );
        if !graphql_operation_name.is_empty() {
            return format!("{graphql_operation_type} {graphql_operation_name}");
        }

        return graphql_operation_type;
    }

    // Database
    let database_system = get_otel_attribute_value_as_string_from_resource_or_span(
        otel_res,
        otel_span,
        DB_SYSTEM_NAME,
        false,
    );
    if !database_system.is_empty() {
        let database_query = get_otel_attribute_value_as_string_from_resource_or_span(
            otel_res,
            otel_span,
            DB_QUERY_TEXT,
            false,
        );
        if !database_query.is_empty() {
            return database_query;
        }

        return database_system;
    }

    // If nothing matches, return Span name
    otel_span.name.clone()
}

fn get_dd_span_type_from_otel_kind(otel_span: &OtelSpan, otel_res: &OtelResource) -> String {
    match otel_span.kind() {
        SpanKind::Server => "web".to_string(),
        SpanKind::Client => {
            let db_system = get_otel_attribute_value_as_string_from_resource_or_span(
                otel_res,
                otel_span,
                DB_SYSTEM_NAME,
                true,
            );
            if db_system.is_empty() {
                return "http".to_string();
            }

            match db_system.as_str() {
                "redis" | "memcached" => "cache".to_string(),
                _ => "db".to_string(),
            }
        }
        _ => "custom".to_string(),
    }
}

fn get_dd_span_kind_from_otel_kind(otel_span: &OtelSpan) -> String {
    match otel_span.kind() {
        SpanKind::Server => "server".to_string(),
        SpanKind::Client => "client".to_string(),
        SpanKind::Consumer => "consumer".to_string(),
        SpanKind::Producer => "producer".to_string(),
        SpanKind::Internal => "internal".to_string(),
        SpanKind::Unspecified => "unspecified".to_string(),
    }
}

fn get_otel_status_code(otel_span: &OtelSpan) -> u32 {
    let mut status_code = get_otel_attribute_value_as_string(
        &otel_span.attributes,
        "http.response.status_code",
        false,
    );
    if !status_code.is_empty() {
        if let Ok(status_code) = status_code.parse::<u32>() {
            return status_code;
        }
    }

    status_code =
        get_otel_attribute_value_as_string(&otel_span.attributes, HTTP_RESPONSE_STATUS_CODE, false);
    if !status_code.is_empty() {
        if let Ok(status_code) = status_code.parse::<u32>() {
            return status_code;
        }
    }

    0
}

// todo: use from libdd_trace_utils when public
fn set_top_level_dd_span(span: &mut DatadogSpan, is_top_level: bool) {
    if is_top_level {
        span.metrics.insert("_top_level".into(), 1.0);
    } else {
        span.metrics.remove("_top_level");
    }
}

// todo: use from libdd_trace_utils when public
fn set_measured_dd_span(span: &mut DatadogSpan, measured: bool) {
    if measured {
        span.metrics.remove("_dd.measured");
    } else {
        span.metrics.insert("_dd.measured".into(), 1.0);
    }
}

fn set_otlp_meta_with_semantic_convention_mappings(
    span: &mut DatadogSpan,
    k: &str,
    v: &str,
    ignore_missing_datadog_fields: bool,
) {
    let mapped_key = get_dd_key_for_otlp_attribute(k);

    if mapped_key.is_empty() {
        return;
    }

    if META_KEYS_TO_DD_SEMANTIC_KEYS.get(k).is_some()
        && (span.meta.contains_key(k) || ignore_missing_datadog_fields)
    {
        return;
    }

    set_otlp_meta(span, k, v);
}

fn set_otlp_metrics_with_semantic_convention_mappings(
    span: &mut DatadogSpan,
    k: &str,
    v: f64,
    ignore_missing_datadog_fields: bool,
) {
    let mapped_key = get_dd_key_for_otlp_attribute(k);

    if mapped_key.is_empty() {
        return;
    }

    if META_KEYS_TO_DD_SEMANTIC_KEYS.get(k).is_some()
        && (span.metrics.contains_key(k) || ignore_missing_datadog_fields)
    {
        return;
    }

    set_otlp_metrics(span, k, v);
}

fn get_dd_key_for_otlp_attribute(k: &str) -> String {
    if k.starts_with("http.request.header.") {
        return format!(
            "http.request.headers.{}",
            k.trim_start_matches("http.request.header.")
        );
    }

    if !is_datadog_apm_convention_key(k) {
        return k.to_string();
    }

    String::new()
}

fn is_datadog_apm_convention_key(k: &str) -> bool {
    k == "service.name"
        || k == "operation.name"
        || k == "resource.name"
        || k == "span.type"
        || k.starts_with("datadog.")
}

fn set_otlp_meta(span: &mut DatadogSpan, k: &str, v: &str) {
    match k {
        "operation.name" => {
            span.name = v.to_string();
        }
        "service.name" => {
            span.service = v.to_string();
        }
        "resource.name" => {
            span.resource = v.to_string();
        }
        "span.type" => {
            span.r#type = v.to_string();
        }
        "analytics.event" => {
            if let Ok(b) = v.parse::<bool>() {
                let value = if b { 1.0 } else { 0.0 };
                span.metrics
                    .insert(KEY_SAMPLING_RATE_EVENT_EXTRACTION.into(), value);
            }
        }
        _ => {
            span.meta.insert(k.to_string(), v.to_string());
        }
    }
}

fn set_otlp_metrics(span: &mut DatadogSpan, k: &str, v: f64) {
    match k {
        "sampling.priority" => {
            span.metrics.insert(KEY_SAMPLING_PRIORITY_V1.into(), v);
        }
        _ => {
            span.metrics.insert(k.to_string(), v);
        }
    }
}

fn marshal_events(events: &[Event]) -> String {
    let mut json_events = Vec::new();

    for event in events {
        let mut event_obj = json!({});

        if event.time_unix_nano != 0 {
            event_obj["time_unix_nano"] = json!(event.time_unix_nano);
        }

        if !event.name.is_empty() {
            event_obj["name"] = json!(event.name);
        }

        if !event.attributes.is_empty() {
            let mut attrs = json!({});
            for kv in &event.attributes {
                let key = kv.key.to_string();
                if let Some(v) = &kv.value {
                    if let Some(value) = &v.value {
                        attrs[key] = json!(otel_value_to_string(value));
                    }
                }
            }
            event_obj["attributes"] = attrs;
        }

        if event.dropped_attributes_count > 0 {
            event_obj["dropped_attributes_count"] = json!(event.dropped_attributes_count);
        }

        json_events.push(event_obj);
    }

    serde_json::to_string(&json_events).unwrap_or_default()
}

fn marshal_links(links: &[Link]) -> String {
    let mut json_links = Vec::new();

    for link in links {
        let mut link_obj = json!({});

        link_obj["trace_id"] = json!(hex::encode(&link.trace_id));
        link_obj["span_id"] = json!(hex::encode(&link.span_id));

        if !link.trace_state.is_empty() {
            link_obj["tracestate"] = json!(link.trace_state);
        }

        if !link.attributes.is_empty() {
            let mut attrs = json!({});
            for kv in &link.attributes {
                let key = kv.key.to_string();
                if let Some(v) = &kv.value {
                    if let Some(value) = &v.value {
                        attrs[key] = json!(otel_value_to_string(value));
                    }
                }
            }
            link_obj["attributes"] = attrs;
        }

        if link.dropped_attributes_count > 0 {
            link_obj["dropped_attributes_count"] = json!(link.dropped_attributes_count);
        }

        json_links.push(link_obj);
    }

    serde_json::to_string(&json_links).unwrap_or_default()
}

fn set_span_events_has_exception(dd_span: &mut DatadogSpan, otel_span: &OtelSpan) {
    for event in &otel_span.events {
        if event.name == "exception" {
            dd_span.meta.insert(
                "_dd.span_events.has_exception".to_string(),
                "true".to_string(),
            );
            return;
        }
    }
}

fn set_span_error_from_otel_span(dd_span: &mut DatadogSpan, otel_span: &OtelSpan) {
    let status = &otel_span.status;

    if status
        .as_ref()
        .is_some_and(|s| s.code != StatusCode::Error as i32)
    {
        return;
    }

    for event in &otel_span.events {
        if event.name != "exception" {
            continue;
        }

        let error_message =
            get_otel_attribute_value_as_string(&event.attributes, EXCEPTION_MESSAGE, false);
        if !error_message.is_empty() {
            dd_span.meta.insert("error.msg".to_string(), error_message);
        }

        let error_type =
            get_otel_attribute_value_as_string(&event.attributes, EXCEPTION_TYPE, false);
        if !error_type.is_empty() {
            dd_span.meta.insert("error.type".to_string(), error_type);
        }

        let error_stack =
            get_otel_attribute_value_as_string(&event.attributes, EXCEPTION_STACKTRACE, false);
        if !error_stack.is_empty() {
            dd_span.meta.insert("error.stack".to_string(), error_stack);
        }
    }

    if !dd_span.meta.contains_key("error.msg") {
        if let Some(status) = status {
            dd_span
                .meta
                .insert("error.msg".to_string(), status.message.clone());
            return;
        }

        // TODO(duncanista): this diverges from Go, it seems there is a lot of inconsistency in this area
        if let Some(status_code) = dd_span.meta.get("http.response.status_code") {
            dd_span
                .meta
                .insert("error.msg".to_string(), status_code.to_string());
        }
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_possible_wrap)]
pub fn otel_span_to_dd_span(
    otel_span: &OtelSpan,
    otel_res: &OtelResource,
    lib: &OtelInstrumentationScope,
    config: Arc<Config>,
) -> DatadogSpan {
    let (trace_id_lower_order_bits, trace_id_higher_order_bits) =
        otel_trace_id_to_dd_id(&otel_span.trace_id);
    let meta = HashMap::from([(
        DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY.to_string(),
        trace_id_higher_order_bits.to_string(),
    )]);
    let mut dd_span = DatadogSpan {
        service: get_otel_attribute_value_as_string(
            &otel_span.attributes,
            KEY_DATADOG_SERVICE,
            true,
        ),
        name: get_otel_attribute_value_as_string(&otel_span.attributes, KEY_DATADOG_NAME, true),
        resource: get_otel_attribute_value_as_string(
            &otel_res.attributes,
            KEY_DATADOG_RESOURCE,
            true,
        ),
        r#type: get_otel_attribute_value_as_string(&otel_span.attributes, KEY_DATADOG_TYPE, true),
        trace_id: trace_id_lower_order_bits,
        span_id: otel_span_id_to_u64(&otel_span.span_id),
        parent_id: otel_span_id_to_u64(&otel_span.parent_span_id),
        start: otel_span.start_time_unix_nano as i64,
        duration: (otel_span.end_time_unix_nano - otel_span.start_time_unix_nano) as i64,
        error: 0,
        meta,
        metrics: HashMap::new(),
        meta_struct: HashMap::new(),
        span_links: Vec::new(),
        span_events: Vec::new(),
    };

    // Set error status
    if let Some(error_value) = get_otel_attribute_value(&otel_span.attributes, KEY_DATADOG_ERROR) {
        let string_value = serde_json::to_string(&error_value).unwrap_or_default();
        let value = serde_json::from_str::<i32>(&string_value).unwrap_or(0);
        dd_span.error = value;
    } else if otel_span
        .status
        .as_ref()
        .is_some_and(|s| s.code == StatusCode::Error as i32)
    {
        dd_span.error = 1;
    }

    // Set kind
    let kind =
        get_otel_attribute_value_as_string(&otel_span.attributes, KEY_DATADOG_SPAN_KIND, true);
    if !kind.is_empty() {
        dd_span.meta.insert("span.kind".to_string(), kind);
    }

    if !config.otlp_config_ignore_missing_datadog_fields {
        if dd_span.service.is_empty() {
            dd_span.service = get_otel_service(otel_res, true);
        }

        if otel_operation_and_resource_v2_enabled(config.clone()) {
            dd_span.name = get_otel_operation_name_v2(otel_span, lib);
        } else {
            dd_span.name = get_otel_operation_name_v1(
                otel_span,
                lib,
                config.otlp_config_traces_span_name_as_resource_name,
                &config.otlp_config_traces_span_name_remappings,
                true,
            );
        }

        if dd_span.resource.is_empty() {
            dd_span.resource = get_otel_resource(otel_span, otel_res);
        }

        if dd_span.r#type.is_empty() {
            dd_span.r#type = get_otel_attribute_value_as_string_from_resource_or_span(
                otel_res,
                otel_span,
                "span.type",
                true,
            );
            if dd_span.r#type.is_empty() {
                dd_span.r#type = get_dd_span_type_from_otel_kind(otel_span, otel_res);
            }
        }

        if !dd_span.meta.contains_key("span.type") {
            dd_span.meta.insert(
                "span.type".to_string(),
                get_dd_span_kind_from_otel_kind(otel_span),
            );
        }

        let mut http_status_code = 0;
        let http_status_code_str = get_otel_attribute_value_as_string(
            &otel_span.attributes,
            KEY_DATADOG_HTTP_STATUS_CODE,
            false,
        );
        if http_status_code_str.is_empty() {
            http_status_code = get_otel_status_code(otel_span);
        } else if let Ok(status_code) = http_status_code_str.parse::<u32>() {
            http_status_code = status_code;
        }

        if http_status_code != 0 {
            dd_span
                .metrics
                .insert("http.status_code".to_string(), f64::from(http_status_code));
        }

        if !dd_span.meta.contains_key("env") {
            let env = get_otel_attribute_value_as_string(
                &otel_res.attributes,
                DEPLOYMENT_ENVIRONMENT_NAME,
                true,
            );

            if !env.is_empty() {
                dd_span.meta.insert("env".to_string(), env);
            }
        }
    }

    let top_level_by_kind = config
        .apm_features
        .contains(&"enable_otlp_compute_top_level_by_span_kind".to_string());
    let is_top_level = if top_level_by_kind {
        otel_span.parent_span_id.is_empty()
            || otel_span.kind() == SpanKind::Server
            || otel_span.kind() == SpanKind::Consumer
    } else {
        false
    };

    if is_top_level {
        set_top_level_dd_span(&mut dd_span, true);
    }

    let is_measured =
        get_otel_attribute_value_as_string(&otel_span.attributes, "_dd.measured", false) == "1";
    if is_measured || otel_span.kind() == SpanKind::Client || otel_span.kind() == SpanKind::Producer
    {
        set_measured_dd_span(&mut dd_span, true);
    }

    for (dd_semantic_key, dd_meta_key) in DD_SEMANTIC_KEYS_TO_META_KEYS.iter() {
        let value =
            get_otel_attribute_value_as_string(&otel_span.attributes, dd_semantic_key, false);
        if !value.is_empty() {
            dd_span.meta.insert((*dd_meta_key).to_string(), value);
        }
    }

    for KeyValue { key, value } in &otel_res.attributes {
        if let Some(v) = value.as_ref().and_then(|v| v.value.as_ref()) {
            let value = otel_value_to_string(v);
            if !value.is_empty() {
                set_otlp_meta_with_semantic_convention_mappings(
                    &mut dd_span,
                    key,
                    &value,
                    config.otlp_config_ignore_missing_datadog_fields,
                );
            }
        }
    }

    for KeyValue { key, value } in &lib.attributes {
        if let Some(v) = value.as_ref().and_then(|v| v.value.as_ref()) {
            let value = otel_value_to_string(v);
            if !value.is_empty() {
                dd_span.meta.insert(key.to_string(), value);
            }
        }
    }

    dd_span.meta.insert(
        "otel.trace_id".to_string(),
        hex::encode(&otel_span.trace_id),
    );
    if dd_span.meta.contains_key("version") {
        let service_version =
            get_otel_attribute_value_as_string(&otel_res.attributes, SERVICE_VERSION, false);
        if !service_version.is_empty() {
            dd_span.meta.insert("version".to_string(), service_version);
        }
    }

    set_span_events_has_exception(&mut dd_span, otel_span);

    if !otel_span.events.is_empty() {
        dd_span
            .meta
            .insert("events".to_string(), marshal_events(&otel_span.events));
    }

    if !otel_span.links.is_empty() {
        dd_span.meta.insert(
            "_dd.span_links".to_string(),
            marshal_links(&otel_span.links),
        );
    }

    for KeyValue { key, value } in &otel_span.attributes {
        if key.starts_with("datadog.") {
            continue;
        }

        if let Some(v) = value.as_ref().and_then(|v| v.value.as_ref()) {
            match v {
                any_value::Value::DoubleValue(v) => {
                    set_otlp_metrics_with_semantic_convention_mappings(
                        &mut dd_span,
                        key,
                        *v,
                        config.otlp_config_ignore_missing_datadog_fields,
                    );
                }
                any_value::Value::IntValue(v) => {
                    set_otlp_metrics_with_semantic_convention_mappings(
                        &mut dd_span,
                        key,
                        *v as f64,
                        config.otlp_config_ignore_missing_datadog_fields,
                    );
                }
                _ => set_otlp_meta_with_semantic_convention_mappings(
                    &mut dd_span,
                    key,
                    &otel_value_to_string(v),
                    config.otlp_config_ignore_missing_datadog_fields,
                ),
            }
        }
    }

    if !otel_span.trace_state.is_empty() {
        dd_span
            .meta
            .insert("w3c.trace_state".to_string(), otel_span.trace_state.clone());
    }

    if !lib.name.is_empty() {
        dd_span
            .meta
            .insert(OTEL_SCOPE_NAME.to_string(), lib.name.clone());
    }

    if !lib.version.is_empty() {
        dd_span
            .meta
            .insert(OTEL_SCOPE_VERSION.to_string(), lib.version.clone());
    }

    if let Some(status) = &otel_span.status {
        dd_span
            .meta
            .insert(OTEL_STATUS_CODE.to_string(), status.code.to_string());
        dd_span
            .meta
            .insert(OTEL_STATUS_DESCRIPTION.to_string(), status.message.clone());
    }

    if config.otlp_config_ignore_missing_datadog_fields
        && (!dd_span.meta.contains_key("error.msg")
            || !dd_span.meta.contains_key("error.type")
            || !dd_span.meta.contains_key("error.stack"))
    {
        set_span_error_from_otel_span(&mut dd_span, otel_span);
    }

    dd_span
}

pub fn otel_resource_spans_to_dd_spans(
    resource_spans: &ResourceSpans,
    config: Arc<Config>,
) -> Vec<Vec<DatadogSpan>> {
    let mut dd_spans = Vec::new();
    let Some(otel_res) = resource_spans.resource.as_ref() else {
        debug!("Resource spans have no resource, skipping");
        return dd_spans;
    };

    let mut traces_by_id: HashMap<u64, Vec<DatadogSpan>> = HashMap::new();

    for scope_spans in &resource_spans.scope_spans {
        let otel_spans: &Vec<OtelSpan> = &scope_spans.spans;
        let Some(lib) = scope_spans.scope.as_ref() else {
            debug!("Scope spans have no instrumentation scope, skipping");
            continue;
        };

        for otel_span in otel_spans {
            let (trace_id_lower, _) = otel_trace_id_to_dd_id(&otel_span.trace_id);
            let dd_span = otel_span_to_dd_span(otel_span, otel_res, lib, config.clone());
            traces_by_id.entry(trace_id_lower).or_default();

            traces_by_id
                .get_mut(&trace_id_lower)
                .expect("infallible")
                .push(dd_span);
        }
    }

    // TODO(duncanista): commented out code is not used since it should be set as the tracer header tags
    // figure out a way to set them later.
    //
    // let lang =
    //     get_otel_attribute_value_as_string(&otel_res.attributes, TELEMETRY_SDK_LANGUAGE, true);
    // let tracer_version = format!(
    //     "otlp-{}",
    //     get_otel_attribute_value_as_string(&otel_res.attributes, TELEMETRY_SDK_VERSION, true,)
    // );

    // // hostname

    // let mut container_id = get_otel_attribute_value_as_string(
    //     &otel_res.attributes,
    //     KEY_DATADOG_CONTAINER_ID,
    //     true,
    // );
    // if container_id.is_empty() {
    //     container_id =
    //         get_otel_attribute_value_as_string(&otel_res.attributes, CONTAINER_ID, true);
    // }

    // let mut env = get_otel_attribute_value_as_string(&otel_res.attributes, KEY_DATADOG_ENVIRONMENT, true);
    // if env.is_empty() && !self.config.otlp_config_ignore_missing_datadog_fields {
    //     env = get_otel_attribute_value_as_string(&otel_res.attributes, DEPLOYMENT_ENVIRONMENT, true);
    // }

    // let tracer_header_tags = DatadogTracerHeaderTags {
    //     lang: &lang,
    //     lang_version: "",
    //     lang_interpreter: "",
    //     lang_vendor: "",
    //     tracer_version: &tracer_version,
    //     container_id: &container_id,
    //     client_computed_top_level:
    //         config
    //         .apm_features
    //         .contains(&"enable_otlp_compute_top_level_by_span_kind".to_string()),
    //     client_computed_stats: false,
    //     dropped_p0_traces: 0,
    //     dropped_p0_spans: 0,
    // };

    for (_, spans) in traces_by_id {
        dd_spans.push(spans);
    }

    dd_spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, ArrayValue, KeyValue, KeyValueList};

    #[test]
    fn test_otel_value_to_string_string_value() {
        let value = Value::StringValue("test_string".to_string());
        assert_eq!(otel_value_to_string(&value), "test_string");
    }

    #[test]
    fn test_otel_value_to_string_bool_value() {
        let value = Value::BoolValue(true);
        assert_eq!(otel_value_to_string(&value), "true");

        let value = Value::BoolValue(false);
        assert_eq!(otel_value_to_string(&value), "false");
    }

    #[test]
    fn test_otel_value_to_string_int_value() {
        let value = Value::IntValue(42);
        assert_eq!(otel_value_to_string(&value), "42");

        let value = Value::IntValue(-123);
        assert_eq!(otel_value_to_string(&value), "-123");

        let value = Value::IntValue(0);
        assert_eq!(otel_value_to_string(&value), "0");
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn test_otel_value_to_string_double_value() {
        let value = Value::DoubleValue(3.14);
        assert_eq!(otel_value_to_string(&value), "3.14");

        let value = Value::DoubleValue(-2.5);
        assert_eq!(otel_value_to_string(&value), "-2.5");

        let value = Value::DoubleValue(0.0);
        assert_eq!(otel_value_to_string(&value), "0");
    }

    #[test]
    fn test_otel_value_to_string_bytes_value() {
        let value = Value::BytesValue(vec![0x01, 0x02, 0x03, 0x04]);
        assert_eq!(otel_value_to_string(&value), "01020304");

        let value = Value::BytesValue(vec![]);
        assert_eq!(otel_value_to_string(&value), "");
    }

    #[test]
    fn test_otel_value_to_string_array_value() {
        let array = vec![
            AnyValue {
                value: Some(Value::StringValue("one".to_string())),
            },
            AnyValue {
                value: Some(Value::IntValue(2)),
            },
            AnyValue {
                value: Some(Value::BoolValue(true)),
            },
        ];

        let value = Value::ArrayValue(ArrayValue { values: array });
        let result = otel_value_to_string(&value);

        let expected = r#"["one","2","true"]"#;
        assert_eq!(result, expected);
    }

    #[test]
    fn test_otel_value_to_string_kvlist_value() {
        let kvlist = vec![
            KeyValue {
                key: "key1".to_string(),
                value: Some(AnyValue {
                    value: Some(Value::StringValue("value1".to_string())),
                }),
            },
            KeyValue {
                key: "key2".to_string(),
                value: Some(AnyValue {
                    value: Some(Value::IntValue(42)),
                }),
            },
        ];

        let value = Value::KvlistValue(KeyValueList { values: kvlist });
        let result = otel_value_to_string(&value);

        // The result should be a JSON object with key-value pairs
        // Note: The order of keys in a JSON object is not guaranteed
        assert!(result.contains("\"key1\":\"value1\""));
        assert!(result.contains("\"key2\":\"42\""));
        assert!(result.starts_with('{'));
        assert!(result.ends_with('}'));
    }

    #[test]
    fn test_otel_value_to_string_empty_array() {
        let value = Value::ArrayValue(ArrayValue { values: vec![] });
        assert_eq!(otel_value_to_string(&value), "[]");
    }

    #[test]
    fn test_otel_value_to_string_empty_kvlist() {
        let value = Value::KvlistValue(KeyValueList { values: vec![] });
        assert_eq!(otel_value_to_string(&value), "{}");
    }

    #[test]
    fn test_otel_operation_name() {
        let otel_span = OtelSpan {
            name: "test-span".to_string(),
            kind: SpanKind::Server as i32,
            ..Default::default()
        };
        let lib = OtelInstrumentationScope {
            name: "opentelemetry_instrumentation_aws_lambda".to_string(),
            version: String::new(),
            attributes: [].to_vec(),
            dropped_attributes_count: 0,
        };

        assert_eq!(
            get_otel_operation_name_v1(&otel_span, &lib, false, &HashMap::new(), true),
            "opentelemetry_instrumentation_aws_lambda.server"
        );
        // With a non-matching lib name, should return server.request
        assert_eq!(get_otel_operation_name_v2(&otel_span, &lib), "server.request");
    }

    #[test]
    fn test_otel_operation_name_aws_lambda() {
        // Test that AWS Lambda OTEL instrumentation gets aws.lambda operation name
        let otel_span = OtelSpan {
            name: "handler.handler".to_string(),
            kind: SpanKind::Server as i32,
            ..Default::default()
        };
        let aws_lambda_lib = OtelInstrumentationScope {
            name: "opentelemetry.instrumentation.aws_lambda".to_string(),
            version: "0.42b0".to_string(),
            attributes: [].to_vec(),
            dropped_attributes_count: 0,
        };

        // AWS Lambda Server span should return aws.lambda
        assert_eq!(
            get_otel_operation_name_v2(&otel_span, &aws_lambda_lib),
            "aws.lambda"
        );

        // Non-server span from AWS Lambda instrumentation should NOT return aws.lambda
        let internal_span = OtelSpan {
            name: "my-function".to_string(),
            kind: SpanKind::Internal as i32,
            ..Default::default()
        };
        assert_eq!(
            get_otel_operation_name_v2(&internal_span, &aws_lambda_lib),
            "SPAN_KIND_INTERNAL"
        );

        // Server span from different instrumentation should return server.request
        let other_lib = OtelInstrumentationScope {
            name: "handler".to_string(),
            version: String::new(),
            attributes: [].to_vec(),
            dropped_attributes_count: 0,
        };
        assert_eq!(
            get_otel_operation_name_v2(&otel_span, &other_lib),
            "server.request"
        );
    }
}
