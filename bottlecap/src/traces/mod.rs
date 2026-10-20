// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod data_streams;
pub mod http_client;
pub mod propagation;
pub mod proxy_aggregator;
pub mod proxy_flusher;
pub mod span_dedup;
pub mod span_dedup_service;
pub mod span_pointers;
pub mod stats_aggregator;
pub mod stats_concentrator_service;
pub mod stats_flusher;
pub mod stats_generator;
pub mod stats_processor;
pub mod trace_agent;
pub mod trace_aggregator;
pub mod trace_aggregator_service;
pub mod trace_flusher;
pub mod trace_processor;

// URL for a call to the Lambda runtime API. The value may be replaced if `AWS_LAMBDA_RUNTIME_API` is set.
const LAMBDA_RUNTIME_URL_PREFIX: &str = "http://127.0.0.1:9001";

// URL for a call from the Datadog Lambda Library to the Lambda Extension
const LAMBDA_EXTENSION_URL_PREFIX: &str = "http://127.0.0.1:8124";

// the first part of a URL for a call from Statsd
const LAMBDA_STATSD_URL_PREFIX: &str = "http://127.0.0.1:8125";

// the first part of a URL from the non-routable address for DNS traces
const DNS_NON_ROUTABLE_ADDRESS_URL_PREFIX: &str = "0.0.0.0";

// the first part of a URL from the localhost address for DNS traces
const DNS_LOCAL_HOST_ADDRESS_URL_PREFIX: &str = "127.0.0.1";

// URL from the `_AWS_XRAY_DAEMON_ADDRESS` for DNS traces
const AWS_XRAY_DAEMON_ADDRESS_URL_PREFIX: &str = "169.254.79.129";

/// The trace intake path the config crate appends to `DD_APM_DD_URL` to build
/// `apm_dd_url`. Must be stripped from that field before deriving any
/// non-trace endpoint (stats, DSM) from it.
pub const TRACE_INTAKE_ROUTE: &str = "/api/v0.2/traces";

/// Returns the base URL of a resolved trace intake for non-trace endpoints.
#[must_use]
pub fn intake_base_url(apm_dd_url: &str) -> &str {
    apm_dd_url
        .trim_end_matches('/')
        .trim_end_matches(TRACE_INTAKE_ROUTE)
}

// Name of the placeholder invocation span set by Java and Go tracers
pub(crate) const INVOCATION_SPAN_RESOURCE: &str = "dd-tracer-serverless-span";

#[allow(clippy::doc_markdown)]
/// Header used for additional tags when sending APM data to the Datadog intake
///
/// Used when we are appending Lambda specific tags to incoming APM data from
/// products like DSM, Profiling, LLMObs, Live Debugger, and more.
const DD_ADDITIONAL_TAGS_HEADER: &str = "X-Datadog-Additional-Tags";

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::intake_base_url;

    #[test]
    fn intake_base_url_strips_trace_route_and_trailing_slashes() {
        let cases = [
            (
                "http://localhost:8080/api/v0.2/traces",
                "http://localhost:8080",
            ),
            (
                "http://localhost:8080/api/v0.2/traces///",
                "http://localhost:8080",
            ),
            (
                "https://trace.agent.datadoghq.com/api/v0.2/traces",
                "https://trace.agent.datadoghq.com",
            ),
            (
                "http://localhost:8080/prefix/api/v0.2/traces",
                "http://localhost:8080/prefix",
            ),
            ("http://localhost:8080/", "http://localhost:8080"),
            ("http://localhost:8080", "http://localhost:8080"),
        ];
        for (input, expected) in cases {
            assert_eq!(intake_base_url(input), expected, "input: {input:?}");
        }
    }
}
