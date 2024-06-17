// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;
use log::{error, info};
use std::cmp::Ordering;
use std::collections::HashMap;

pub use datadog_trace_utils::send_data::SendData;
// TODO: EK - FIX THIS
pub use datadog_trace_utils::send_data::send_data_result::SendDataResult;
pub use datadog_trace_utils::tracer_header_tags::TracerHeaderTags;
use datadog_trace_normalization::normalizer;
use datadog_trace_protobuf::pb::{self, Span, TraceChunk, TracerPayload};
use tokio_util::bytes::Buf;

/// Span metric the mini agent must set for the backend to recognize top level span
const TOP_LEVEL_KEY: &str = "_top_level";
/// Span metric the tracer sets to denote a top level span
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";

const MAX_PAYLOAD_SIZE: usize = 50 * 1024 * 1024;

#[derive(Error, Debug)]
pub enum TraceReadError {
    #[error("Error deserializing trace from request body: {msg}")]
    ParseError {
        msg: String
    },
    #[error("No traces in body")]
    NoTraces
}

/// First value of returned tuple is the payload size
pub async fn get_traces_from_request_body(body: impl Buf) -> Result<(usize, Vec<Vec<Span>>), TraceReadError> {
    let size = body.remaining();

    let traces: Vec<Vec<Span>> = match rmp_serde::from_read(body.reader()) {
        Ok(res) => res,
        Err(err) => {
            return Err(TraceReadError::ParseError {
                msg: err.to_string()
            })
        }
    };

    if traces.is_empty() {
        return Err(TraceReadError::NoTraces);
    }

    Ok((size, traces))
}

// Tags gathered from a trace's root span
#[derive(Default)]
pub struct RootSpanTags<'a> {
    pub env: &'a str,
    pub app_version: &'a str,
    pub hostname: &'a str,
    pub runtime_id: &'a str,
}

pub(crate) fn construct_trace_chunk(trace: Vec<Span>) -> TraceChunk {
    TraceChunk {
        priority: normalizer::SamplerPriority::None as i32,
        origin: "".to_string(),
        spans: trace,
        tags: HashMap::new(),
        dropped_trace: false,
    }
}

pub(crate) fn construct_tracer_payload(
    chunks: Vec<TraceChunk>,
    tracer_tags: &TracerHeaderTags,
    root_span_tags: RootSpanTags,
) -> TracerPayload {
    TracerPayload {
        app_version: root_span_tags.app_version.to_string(),
        language_name: tracer_tags.lang.to_string(),
        container_id: tracer_tags.container_id.to_string(),
        env: root_span_tags.env.to_string(),
        runtime_id: root_span_tags.runtime_id.to_string(),
        chunks,
        hostname: root_span_tags.hostname.to_string(),
        language_version: tracer_tags.lang_version.to_string(),
        tags: HashMap::new(),
        tracer_version: tracer_tags.tracer_version.to_string(),
    }
}

fn cmp_send_data_payloads(a: &pb::TracerPayload, b: &pb::TracerPayload) -> Ordering {
    a.tracer_version
        .cmp(&b.tracer_version)
        .then(a.language_version.cmp(&b.language_version))
        .then(a.language_name.cmp(&b.language_name))
        .then(a.hostname.cmp(&b.hostname))
        .then(a.container_id.cmp(&b.container_id))
        .then(a.runtime_id.cmp(&b.runtime_id))
        .then(a.env.cmp(&b.env))
        .then(a.app_version.cmp(&b.app_version))
}

pub fn coalesce_send_data(mut data: Vec<SendData>) -> Vec<SendData> {
    // TODO trace payloads with identical data except for chunk could be merged?

    data.sort_unstable_by(|a, b| {
        a.get_target()
            .url
            .to_string()
            .cmp(&b.get_target().url.to_string())
    });
    data.dedup_by(|a, b| {
        if a.get_target().url == b.get_target().url {
            // Size is only an approximation. In practice it won't vary much, but be safe here.
            // We also don't care about the exact maximum size, like two 25 MB or one 50 MB request
            // has similar results. The primary goal here is avoiding many small requests.
            // TODO: maybe make the MAX_PAYLOAD_SIZE configurable?
            if a.size + b.size < MAX_PAYLOAD_SIZE / 2 {
                // Note: dedup_by drops a, and retains b.
                b.tracer_payloads.append(&mut a.tracer_payloads);
                b.size += a.size;
                return true;
            }
        }
        false
    });
    // Merge chunks with common properties. Reduces requests for agentful mode.
    // And reduces a little bit of data for agentless.
    for send_data in data.iter_mut() {
        send_data
            .tracer_payloads
            .sort_unstable_by(cmp_send_data_payloads);
        send_data.tracer_payloads.dedup_by(|a, b| {
            if cmp_send_data_payloads(a, b) == Ordering::Equal {
                // Note: dedup_by drops a, and retains b.
                b.chunks.append(&mut a.chunks);
                return true;
            }
            false
        })
    }
    data
}

fn get_root_span_index(trace: &Vec<Span>) -> Result<usize, TraceReadError> {
    if trace.is_empty() {
        return Err(TraceReadError::NoTraces);
    }

    // parent_id -> (child_span, index_of_child_span_in_trace)
    let mut parent_id_to_child_map: HashMap<u64, (&Span, usize)> = HashMap::new();

    // look for the span with parent_id == 0 (starting from the end) since some clients put the root
    // span last.
    for i in (0..trace.len()).rev() {
        let cur_span = &trace[i];
        if cur_span.parent_id == 0 {
            return Ok(i);
        }
        parent_id_to_child_map.insert(cur_span.parent_id, (cur_span, i));
    }

    for span in trace {
        if parent_id_to_child_map.contains_key(&span.span_id) {
            parent_id_to_child_map.remove(&span.span_id);
        }
    }

    // if the trace is valid, parent_id_to_child_map should just have 1 entry at this point.
    if parent_id_to_child_map.len() != 1 {
        error!(
            "Could not find the root span for trace with trace_id: {}",
            &trace[0].trace_id,
        );
    }

    // pick a span without a parent
    let span_tuple = match parent_id_to_child_map.values().copied().next() {
        Some(res) => res,
        None => {
            // just return the index of the last span in the trace.
            info!("Returning index of last span in trace as root span index.");
            return Ok(trace.len() - 1);
        }
    };

    Ok(span_tuple.1)
}

/// Updates all the spans top-level attribute.
/// A span is considered top-level if:
///   - it's a root span
///   - OR its parent is unknown (other part of the code, distributed trace)
///   - OR its parent belongs to another service (in that case it's a "local root" being the highest
///     ancestor of other spans belonging to this service and attached to it).
pub fn compute_top_level_span(trace: &mut [Span]) {
    let mut span_id_to_service: HashMap<u64, String> = HashMap::new();
    for span in trace.iter() {
        span_id_to_service.insert(span.span_id, span.service.clone());
    }
    for span in trace.iter_mut() {
        if span.parent_id == 0 {
            set_top_level_span(span, true);
            continue;
        }
        match span_id_to_service.get(&span.parent_id) {
            Some(parent_span_service) => {
                if !parent_span_service.eq(&span.service) {
                    // parent is not in the same service
                    set_top_level_span(span, true)
                }
            }
            None => {
                // span has no parent in chunk
                set_top_level_span(span, true)
            }
        }
    }
}

fn set_top_level_span(span: &mut Span, is_top_level: bool) {
    if !is_top_level {
        if span.metrics.contains_key(TOP_LEVEL_KEY) {
            span.metrics.remove(TOP_LEVEL_KEY);
        }
        return;
    }
    span.metrics.insert(TOP_LEVEL_KEY.to_string(), 1.0);
}

pub fn set_serverless_root_span_tags(
    span: &mut Span,
    function_name: Option<String>,
    env_type: &EnvironmentType,
) {
    span.r#type = "serverless".to_string();
    let origin_tag = match env_type {
        EnvironmentType::CloudFunction => "cloudfunction",
        EnvironmentType::AzureFunction => "azurefunction",
    };
    span.meta
        .insert("_dd.origin".to_string(), origin_tag.to_string());
    span.meta
        .insert("origin".to_string(), origin_tag.to_string());

    if let Some(function_name) = function_name {
        span.meta.insert("functionname".to_string(), function_name);
    }
}

fn update_tracer_top_level(span: &mut Span) {
    if span.metrics.contains_key(TRACER_TOP_LEVEL_KEY) {
        span.metrics.insert(TOP_LEVEL_KEY.to_string(), 1.0);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentType {
    CloudFunction,
    AzureFunction,
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub struct MiniAgentMetadata {
    pub gcp_project_id: Option<String>,
    pub gcp_region: Option<String>,
}

// pub fn enrich_span_with_mini_agent_metadata(
//     span: &mut Span,
//     mini_agent_metadata: &MiniAgentMetadata,
// ) {
//     if let Some(gcp_project_id) = &mini_agent_metadata.gcp_project_id {
//         span.meta
//             .insert("project_id".to_string(), gcp_project_id.to_string());
//     }
//     if let Some(gcp_region) = &mini_agent_metadata.gcp_region {
//         span.meta
//             .insert("location".to_string(), gcp_region.to_string());
//     }
// }

// pub fn enrich_span_with_azure_metadata(span: &mut Span, mini_agent_version: &str) {
//     if let Some(aas_metadata) = azure_app_services::get_function_metadata() {
//         let aas_tags = [
//             ("aas.resource.id", aas_metadata.get_resource_id()),
//             (
//                 "aas.environment.instance_id",
//                 aas_metadata.get_instance_id(),
//             ),
//             (
//                 "aas.environment.instance_name",
//                 aas_metadata.get_instance_name(),
//             ),
//             ("aas.subscription.id", aas_metadata.get_subscription_id()),
//             ("aas.environment.mini_agent_version", mini_agent_version),
//             ("aas.environment.os", aas_metadata.get_operating_system()),
//             ("aas.resource.group", aas_metadata.get_resource_group()),
//             ("aas.site.name", aas_metadata.get_site_name()),
//             ("aas.site.kind", aas_metadata.get_site_kind()),
//             ("aas.site.type", aas_metadata.get_site_type()),
//         ];
//         aas_tags.into_iter().for_each(|(name, value)| {
//             span.meta.insert(name.to_string(), value.to_string());
//         });
//     }
// }

/// Used to populate root_span_tags fields if they exist in the root span's meta tags
macro_rules! parse_root_span_tags {
    (
        $root_span_meta_map:ident,
        { $($tag:literal => $($root_span_tags_struct_field:ident).+ ,)+ }
    ) => {
        $(
            if let Some(root_span_tag_value) = $root_span_meta_map.get($tag) {
                $($root_span_tags_struct_field).+ = root_span_tag_value;
            }
        )+
    }
}

pub fn collect_trace_chunks(
    mut traces: Vec<Vec<Span>>,
    tracer_header_tags: &TracerHeaderTags,
    process_chunk: impl Fn(&mut TraceChunk, usize),
    is_agentless: bool,
) -> TracerPayload {
    let mut trace_chunks: Vec<TraceChunk> = Vec::new();

    // We'll skip setting the global metadata and rely on the agent to unpack these
    let mut gathered_root_span_tags = !is_agentless;
    let mut root_span_tags = RootSpanTags::default();

    for trace in traces.iter_mut() {
        if is_agentless {
            if let Err(e) = normalizer::normalize_trace(trace) {
                error!("Error normalizing trace: {e}");
            }
        }

        let mut chunk = construct_trace_chunk(trace.to_vec());

        let root_span_index = match get_root_span_index(trace) {
            Ok(res) => res,
            Err(e) => {
                error!("Error getting the root span index of a trace, skipping. {e}");
                continue;
            }
        };

        if let Err(e) = normalizer::normalize_chunk(&mut chunk, root_span_index) {
            error!("Error normalizing trace chunk: {e}");
        }

        for span in chunk.spans.iter_mut() {
            // TODO: obfuscate & truncate spans
            if tracer_header_tags.client_computed_top_level {
                update_tracer_top_level(span);
            }
        }

        if !tracer_header_tags.client_computed_top_level {
            compute_top_level_span(&mut chunk.spans);
        }

        process_chunk(&mut chunk, root_span_index);

        trace_chunks.push(chunk);

        if !gathered_root_span_tags {
            gathered_root_span_tags = true;
            let meta_map = &trace[root_span_index].meta;
            parse_root_span_tags!(
                meta_map,
                {
                    "env" => root_span_tags.env,
                    "version" => root_span_tags.app_version,
                    "_dd.hostname" => root_span_tags.hostname,
                    "runtime-id" => root_span_tags.runtime_id,
                }
            );
        }
    }

    construct_tracer_payload(trace_chunks, tracer_header_tags, root_span_tags)
}

#[cfg(test)]
mod tests {
    use hyper::Request;
    use serde_json::json;
    use std::collections::HashMap;

    use super::{get_root_span_index, set_serverless_root_span_tags};
    use crate::trace_utils::{TracerHeaderTags, MAX_PAYLOAD_SIZE};
    use crate::{
        test_utils::create_test_span,
        trace_utils::{self, SendData},
    };
    use datadog_trace_protobuf::pb::TraceChunk;
    use datadog_trace_protobuf::pb::{Span, TracerPayload};
    use ddcommon::Endpoint;

    #[test]
    fn test_coalescing_does_not_exceed_max_size() {
        let dummy = SendData::new(
            MAX_PAYLOAD_SIZE / 5 + 1,
            TracerPayload {
                container_id: "".to_string(),
                language_name: "".to_string(),
                language_version: "".to_string(),
                tracer_version: "".to_string(),
                runtime_id: "".to_string(),
                chunks: vec![TraceChunk {
                    priority: 0,
                    origin: "".to_string(),
                    spans: vec![],
                    tags: Default::default(),
                    dropped_trace: false,
                }],
                tags: Default::default(),
                env: "".to_string(),
                hostname: "".to_string(),
                app_version: "".to_string(),
            },
            TracerHeaderTags::default(),
            &Endpoint::default(),
        );
        let coalesced = trace_utils::coalesce_send_data(vec![
            dummy.clone(),
            dummy.clone(),
            dummy.clone(),
            dummy.clone(),
            dummy.clone(),
        ]);
        assert_eq!(
            5,
            coalesced
                .iter()
                .map(|s| s
                    .tracer_payloads
                    .iter()
                    .map(|p| p.chunks.len())
                    .sum::<usize>())
                .sum::<usize>()
        );
        // assert some chunks are actually coalesced
        assert!(
            coalesced
                .iter()
                .map(|s| s
                    .tracer_payloads
                    .iter()
                    .map(|p| p.chunks.len())
                    .max()
                    .unwrap())
                .max()
                .unwrap()
                > 1
        );
        assert!(coalesced.len() > 1 && coalesced.len() < 5);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_get_traces_from_request_body() {
        let pairs = vec![
            (
                json!([{
                    "service": "test-service",
                    "name": "test-service-name",
                    "resource": "test-service-resource",
                    "trace_id": 111,
                    "span_id": 222,
                    "parent_id": 333,
                    "start": 1,
                    "duration": 5,
                    "error": 0,
                    "meta": {},
                    "metrics": {},
                }]),
                vec![vec![Span {
                    service: "test-service".to_string(),
                    name: "test-service-name".to_string(),
                    resource: "test-service-resource".to_string(),
                    trace_id: 111,
                    span_id: 222,
                    parent_id: 333,
                    start: 1,
                    duration: 5,
                    error: 0,
                    meta: HashMap::new(),
                    metrics: HashMap::new(),
                    meta_struct: HashMap::new(),
                    r#type: "".to_string(),
                    span_links: vec![],
                }]],
            ),
            (
                json!([{
                    "name": "test-service-name",
                    "resource": "test-service-resource",
                    "trace_id": 111,
                    "span_id": 222,
                    "start": 1,
                    "duration": 5,
                    "meta": {},
                }]),
                vec![vec![Span {
                    service: "".to_string(),
                    name: "test-service-name".to_string(),
                    resource: "test-service-resource".to_string(),
                    trace_id: 111,
                    span_id: 222,
                    parent_id: 0,
                    start: 1,
                    duration: 5,
                    error: 0,
                    meta: HashMap::new(),
                    metrics: HashMap::new(),
                    meta_struct: HashMap::new(),
                    r#type: "".to_string(),
                    span_links: vec![],
                }]],
            ),
        ];

        for (trace_input, output) in pairs {
            let bytes = rmp_serde::to_vec(&vec![&trace_input]).unwrap();
            let request = Request::builder()
                .body(hyper::body::Body::from(bytes))
                .unwrap();
            let res = trace_utils::get_traces_from_request_body(request.into_body()).await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap().1, output);
        }
    }

    #[test]
    fn test_get_root_span_index_from_complete_trace() {
        let trace = vec![
            create_test_span(1234, 12341, 0, 1, false),
            create_test_span(1234, 12342, 12341, 1, false),
            create_test_span(1234, 12343, 12342, 1, false),
        ];

        let root_span_index = get_root_span_index(&trace);
        assert!(root_span_index.is_ok());
        assert_eq!(root_span_index.unwrap(), 0);
    }

    #[test]
    fn test_get_root_span_index_from_partial_trace() {
        let trace = vec![
            create_test_span(1234, 12342, 12341, 1, false),
            create_test_span(1234, 12341, 12340, 1, false), /* this is the root span, it's
                                                             * parent is not in the trace */
            create_test_span(1234, 12343, 12342, 1, false),
        ];

        let root_span_index = get_root_span_index(&trace);
        assert!(root_span_index.is_ok());
        assert_eq!(root_span_index.unwrap(), 1);
    }

    #[test]
    fn test_set_serverless_root_span_tags_azure_function() {
        let mut span = create_test_span(1234, 12342, 12341, 1, false);
        set_serverless_root_span_tags(
            &mut span,
            Some("test_function".to_string()),
            &trace_utils::EnvironmentType::AzureFunction,
        );
        assert_eq!(
            span.meta,
            HashMap::from([
                (
                    "runtime-id".to_string(),
                    "test-runtime-id-value".to_string()
                ),
                ("_dd.origin".to_string(), "azurefunction".to_string()),
                ("origin".to_string(), "azurefunction".to_string()),
                ("functionname".to_string(), "test_function".to_string()),
                ("env".to_string(), "test-env".to_string()),
                ("service".to_string(), "test-service".to_string())
            ]),
        );
        assert_eq!(span.r#type, "serverless".to_string())
    }

    #[test]
    fn test_set_serverless_root_span_tags_cloud_function() {
        let mut span = create_test_span(1234, 12342, 12341, 1, false);
        set_serverless_root_span_tags(
            &mut span,
            Some("test_function".to_string()),
            &trace_utils::EnvironmentType::CloudFunction,
        );
        assert_eq!(
            span.meta,
            HashMap::from([
                (
                    "runtime-id".to_string(),
                    "test-runtime-id-value".to_string()
                ),
                ("_dd.origin".to_string(), "cloudfunction".to_string()),
                ("origin".to_string(), "cloudfunction".to_string()),
                ("functionname".to_string(), "test_function".to_string()),
                ("env".to_string(), "test-env".to_string()),
                ("service".to_string(), "test-service".to_string())
            ]),
        );
        assert_eq!(span.r#type, "serverless".to_string())
    }
}
