use std::collections::HashMap;
use std::num::NonZero;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bottlecap::appsec::processor::Processor;
use bottlecap::config::Config;
use bottlecap::lifecycle::invocation::triggers::IdentifiedTrigger;
use bytes::Bytes;
use itertools::Itertools;
use libdd_trace_protobuf::pb::Span;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

#[tokio::test]
async fn test_processor() {
    macro_rules! request_payload {
        ($kind:ident = $name:ident) => {
            IdentifiedTrigger::$kind(
                serde_json::from_slice(
                    &include_bytes!(concat!("payloads/", stringify!($name), ".json"))[..],
                )
                .expect("failed to parse request payload"),
            )
        };
    }

    let cfg = Config {
        serverless_appsec_enabled: true,
        appsec_rules: Some(
            PathBuf::from(file!())
                .parent()
                .expect("failed to get parent directory of this file")
                .join("appsec")
                .join("test-ruleset.json")
                .to_string_lossy()
                .to_string(),
        ),
        appsec_waf_timeout: Duration::from_secs(60), // Ample so it does not time out on slow CI hosts
        api_security_sample_delay: Duration::ZERO,   // Sample all requests
        ..Config::default()
    };

    let subject = Arc::new(Mutex::new(
        Processor::with_capacity(&cfg, unsafe { NonZero::new_unchecked(5) })
            .expect("failed to create Processor"),
    ));

    // Send all the requests
    let mut tasks = JoinSet::new();
    for (rid, payload) in [
        (
            "rid-1",
            request_payload!(APIGatewayRestEvent = api_gateway_proxy_event),
        ),
        (
            "rid-2",
            request_payload!(APIGatewayHttpEvent = api_gateway_http_event_parameterized),
        ),
        (
            "rid-3",
            request_payload!(APIGatewayWebSocketEvent = api_gateway_websocket_message_event),
        ),
        (
            "rid-4",
            request_payload!(ALBEvent = application_load_balancer_multivalue_headers),
        ),
        (
            "rid-5",
            request_payload!(LambdaFunctionUrlEvent = lambda_function_url_event),
        ),
        ("rid-unknown", IdentifiedTrigger::Unknown),
    ] {
        let processor = subject.clone();
        tasks.spawn(async move {
            let mut processor = processor.lock().await;
            processor.process_invocation_next(rid, &payload).await;
        });
    }
    // Make sure all the tasks are completed
    tasks.join_all().await;

    // Send all the responses...
    let mut tasks = JoinSet::new();
    for (rid, payload) in [
        ("rid-extraneous", "will not be processed"),
        (
            "rid-1",
            r#"{"statusCode": 200, "headers": {"Content-Type":"application/x-www-form-urlencoded"}, "body": "foo=bar,baz=bat", "isBase64Encoded": false}"#,
        ),
        (
            "rid-2",
            r#"{"statusCode": 404, "headers": {"Content-Type":"text/plain"}, "body":"Not Found", "isBase64Encoded": false}"#,
        ),
        ("rid-3", r#"{"payload": "will be forwarded as-is"}"#),
        (
            "rid-4",
            r#"{"statusCode": 204, "multiValueHeaders": {"Content-Length": ["0"]}}"#,
        ),
        (
            "rid-5",
            r#"{"statusCode": 300, "headers": {"Content-Type":"application/json"}, "body": "{invalid}", "isBase64Encoded": false}"#,
        ),
    ] {
        let processor = subject.clone();
        tasks.spawn(async move {
            let mut processor = processor.lock().await;
            processor
                .process_invocation_result(rid, &Bytes::from(payload))
                .await;
        });
    }
    // Make sure all the tasks are completed
    tasks.join_all().await;

    // Make sure all the relevant side-effects have been aggregated...
    macro_rules! span {
        (
            meta {
                $($tagname:literal : $tagvalue:expr),+$(,)?
            }
            $(metrics {
                $($metricname:literal : $metricvalue:expr),+$(,)?
            })?
        ) => {
            Span {
                meta: HashMap::from([
                    $(($tagname.to_string(), $tagvalue.to_string())),+
                ]),
                $(metrics: HashMap::from([
                    $( ($metricname.to_string(), $metricvalue) ),+
                ]),)?
                ..Span::default()
            }
        }
    }
    let waf_version = libddwaf::get_version().to_string_lossy().to_string();
    /// Marker for metrics we can't statically know the value of, but must be present & > 0...
    const ANY_POSITIVE_VALUE_IS_FINE: f64 = f64::NAN;
    for (rid, expected) in [
        (
            "rid-1",
            span! {
                meta {
                    "request_id": "rid-1",
                    "_dd.appsec.event_rules.version": "1.15.0+redux", // Hard-coded in the test ruleset
                    "_dd.appsec.fp.http.header": "hdr-0010100011-8a1b5aba-12-d7bf5e5b",
                    "_dd.appsec.fp.http.network": "net-2-1000000000",
                    "_dd.appsec.s.req.body": r#"[{"test":[8]}]"#,
                    "_dd.appsec.s.req.headers": r#"[{"accept-encoding":[[[8]],{"len":1}],"cloudfront-viewer-country":[[[8]],{"len":1}],"x-forwarded-port":[[[8]],{"len":1}],"cloudfront-is-smarttv-viewer":[[[8]],{"len":1}],"user-agent":[[[8]],{"len":1}],"x-forwarded-proto":[[[8]],{"len":1}],"cloudfront-forwarded-proto":[[[8]],{"len":1}],"cloudfront-is-tablet-viewer":[[[8]],{"len":1}],"upgrade-insecure-requests":[[[8]],{"len":1}],"x-amz-cf-id":[[[8]],{"len":1}],"via":[[[8]],{"len":1}],"cloudfront-is-mobile-viewer":[[[8]],{"len":1}],"cache-control":[[[8]],{"len":1}],"accept":[[[8]],{"len":1}],"x-forwarded-for":[[[8]],{"len":1}],"host":[[[8]],{"len":1}],"accept-language":[[[8]],{"len":1}],"cloudfront-is-desktop-viewer":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.s.req.params": r#"[{"proxy":[8]}]"#,
                    "_dd.appsec.s.req.query": r#"[{"foo":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.s.res.body": r#"[{"foo":[8]}]"#,
                    "_dd.appsec.s.res.headers": r#"[{"content-type":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.waf.version": waf_version,
                    "_dd.origin": "appsec",
                    "http.method": "POST",
                    "http.request.headers.accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
                    "http.request.headers.user-agent": "Custom User Agent String",
                    "http.response.headers.content-type": "application/x-www-form-urlencoded",
                    "http.status_code": "200",
                    "http.url": "70ixmpl4fl.execute-api.us-east-2.amazonaws.com/prod/path/to/resource",
                }
                metrics {
                    "_dd.appsec.enabled": 1.0,
                    "_dd.appsec.waf.duration": ANY_POSITIVE_VALUE_IS_FINE,
                    "_dd.appsec.waf.timeouts": 0.0,
                }
            },
        ),
        (
            "rid-2",
            span! {
                meta {
                    "request_id": "rid-2",
                    "_dd.appsec.event_rules.version": "1.15.0+redux", // Hard-coded in the test ruleset
                    "_dd.appsec.fp.http.header": "hdr-0000000010-88d4ddc2-5-1e6648af",
                    "_dd.appsec.fp.http.network": "net-1-1000000000",
                    "_dd.appsec.s.req.headers": r#"[{"accept":[[[8]],{"len":1}],"content-length":[[[8]],{"len":1}],"host":[[[8]],{"len":1}],"user-agent":[[[8]],{"len":1}],"x-amzn-trace-id":[[[8]],{"len":1}],"x-forwarded-for":[[[8]],{"len":1}],"x-forwarded-port":[[[8]],{"len":1}],"x-forwarded-proto":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.s.req.params": r#"[{"id":[8]}]"#,
                    "_dd.appsec.s.res.body": r#"[8]"#,
                    "_dd.appsec.s.res.headers": r#"[{"content-type":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.waf.version": waf_version,
                    "_dd.origin": "appsec",
                    "http.method": "GET",
                    "http.request.headers.accept": "*/*",
                    "http.request.headers.user-agent": "curl/8.1.2",
                    "http.request.headers.x-amzn-trace-id": "Root=1-65f49d71-505edb3b69b8abd513cfa08b",
                    "http.response.headers.content-type": "text/plain",
                    "http.status_code": "404",
                    "http.url": "9vj54we5ih.execute-api.sa-east-1.amazonaws.com/user/42",
                }
                metrics {
                    "_dd.appsec.enabled": 1.0,
                    "_dd.appsec.waf.duration": ANY_POSITIVE_VALUE_IS_FINE,
                    "_dd.appsec.waf.timeouts": 0.0,
                }
            },
        ),
        (
            "rid-3",
            span! {
                meta {
                    "request_id": "rid-3",
                    "_dd.appsec.event_rules.version": "1.15.0+redux", // Hard-coded in the test ruleset
                    "_dd.appsec.s.req.body": r#"[{"action":[8],"message":[8]}]"#,
                    "_dd.appsec.s.res.body": r#"[{"payload":[8]}]"#,
                    "_dd.appsec.waf.version": waf_version,
                    "_dd.origin": "appsec",
                    "http.url": "85fj5nw29d.execute-api.eu-west-1.amazonaws.com/$dev",
                }
                metrics {
                    "_dd.appsec.enabled": 1.0,
                    "_dd.appsec.waf.duration": ANY_POSITIVE_VALUE_IS_FINE,
                    "_dd.appsec.waf.timeouts": 0.0,
                }
            },
        ),
        (
            "rid-4",
            span! {
                meta {
                    "request_id": "rid-4",
                    "_dd.appsec.event_rules.version": "1.15.0+redux", // Hard-coded in the test ruleset
                    "_dd.appsec.fp.http.header": "hdr-0110000011-545ea538-7-3fd1d09a",
                    "_dd.appsec.fp.http.network": "net-1-1000000000",
                    "_dd.appsec.s.req.headers": r#"[{"accept":[[[8]],{"len":1}],"accept-encoding":[[[8]],{"len":1}],"accept-language":[[[8]],{"len":1}],"connection":[[[8]],{"len":1}],"host":[[[8]],{"len":1}],"sec-fetch-mode":[[[8]],{"len":1}],"traceparent":[[[8]],{"len":1}],"tracestate":[[[8]],{"len":1}],"user-agent":[[[8]],{"len":1}],"x-amzn-trace-id":[[[8]],{"len":1}],"x-datadog-parent-id":[[[8]],{"len":1}],"x-datadog-sampling-priority":[[[8]],{"len":1}],"x-datadog-tags":[[[8]],{"len":1}],"x-datadog-trace-id":[[[8]],{"len":1}],"x-forwarded-for":[[[8]],{"len":1}],"x-forwarded-port":[[[8]],{"len":1}],"x-forwarded-proto":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.s.res.headers": r#"[{"content-length":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.waf.version": waf_version,
                    "_dd.origin": "appsec",
                    "http.method": "GET",
                    "http.request.headers.accept": "*/*",
                    "http.request.headers.user-agent": "node",
                    "http.request.headers.x-amzn-trace-id": "Root=1-68126c45-01b175997ab51c4c47a2d643",
                    "http.response.headers.content-length": "0",
                    "http.status_code": "204",
                }
                metrics {
                    "_dd.appsec.enabled": 1.0,
                    "_dd.appsec.waf.duration": ANY_POSITIVE_VALUE_IS_FINE,
                    "_dd.appsec.waf.timeouts": 0.0,
                }
            },
        ),
        (
            "rid-5",
            span! {
                meta {
                    "request_id": "rid-5",
                    "_dd.appsec.event_rules.version": "1.15.0+redux", // Hard-coded in the test ruleset
                    "_dd.appsec.fp.http.header": "hdr-0010100011-f97811a1-13-a19ef930",
                    "_dd.appsec.fp.http.network": "net-1-1000000000",
                    "_dd.appsec.s.req.headers": r#"[{"accept":[[[8]],{"len":1}],"accept-encoding":[[[8]],{"len":1}],"accept-language":[[[8]],{"len":1}],"cache-control":[[[8]],{"len":1}],"host":[[[8]],{"len":1}],"pragma":[[[8]],{"len":1}],"sec-ch-ua":[[[8]],{"len":1}],"sec-ch-ua-mobile":[[[8]],{"len":1}],"sec-ch-ua-platform":[[[8]],{"len":1}],"sec-fetch-dest":[[[8]],{"len":1}],"sec-fetch-mode":[[[8]],{"len":1}],"sec-fetch-site":[[[8]],{"len":1}],"sec-fetch-user":[[[8]],{"len":1}],"upgrade-insecure-requests":[[[8]],{"len":1}],"user-agent":[[[8]],{"len":1}],"x-amzn-trace-id":[[[8]],{"len":1}],"x-forwarded-for":[[[8]],{"len":1}],"x-forwarded-port":[[[8]],{"len":1}],"x-forwarded-proto":[[[8]],{"len":1}]}]"#,
                    // No "_dd.appsec.s.res.body" because the body is invalid here...
                    "_dd.appsec.s.res.headers": r#"[{"content-type":[[[8]],{"len":1}]}]"#,
                    "_dd.appsec.waf.version": waf_version,
                    "_dd.origin": "appsec",
                    "http.method": "GET",
                    "http.request.headers.accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.9",
                    "http.request.headers.user-agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/95.0.4638.69 Safari/537.36",
                    "http.request.headers.x-amzn-trace-id": "Root=1-61953929-1ec00c3011062a48477b169e",
                    "http.response.headers.content-type": "application/json",
                    "http.status_code": "300",
                    "http.url": "a8hyhsshac.lambda-url.eu-south-1.amazonaws.com/",
                }
                metrics {
                    "_dd.appsec.enabled": 1.0,
                    "_dd.appsec.waf.duration": ANY_POSITIVE_VALUE_IS_FINE,
                    "_dd.appsec.waf.timeouts": 0.0,
                }
            },
        ),
        (
            "rid-extraneous",
            span! {meta{"request_id": "rid-extraneous"}},
        ),
    ] {
        let mut span = span! {
            meta { "request_id": rid }
        };
        subject.lock().await.process_span(&mut span);
        assert_span_matches(expected, span);
    }

    ////////////////////////////////////////////////////////////////////////////
    // Now testing with security activity
    subject
        .lock()
        .await
        .process_invocation_next(
            "rid-event",
            &request_payload!(APIGatewayHttpEvent = api_gateway_http_event_appsec),
        )
        .await;
    subject
        .lock()
        .await
        .process_invocation_result("rid-event", &Bytes::from(r#"{"statusCode": 200}"#))
        .await;
    let mut span = span! { meta { "request_id": "rid-event" } };
    subject.lock().await.process_span(&mut span);
    assert_span_matches(
        span! {
            meta {
                "request_id": "rid-event",
                "_dd.appsec.event_rules.version": "1.15.0+redux", // Hard-coded in the test ruleset
                "_dd.appsec.fp.http.header": "hdr-0000000010-40b52535-5-1e6648af",
                "_dd.appsec.fp.http.network": "net-1-1000000000",
                "_dd.appsec.fp.session": "ssn--45f15964-7f506709-",
                "_dd.appsec.json": r#"{"triggers":[{"rule":{"id":"ua0-600-12x","name":"Arachni","on_match":[],"tags":{"capec":"1000/118/169","category":"attack_attempt","confidence":"1","cwe":"200","module":"waf","tool_name":"Arachni","type":"attack_tool"}},"rule_matches":[{"operator":"match_regex","operator_value":"^Arachni\\/v","parameters":[{"address":"server.request.headers.no_cookies","highlight":["Arachni/v"],"key_path":["user-agent","0"],"value":"Arachni/v2"}]}]}]}"#,
                "_dd.appsec.s.req.body": r#"[{"hello":[8]}]"#,
                "_dd.appsec.s.req.cookies": r#"[{"cookie1":[[[8]],{"len":1}],"cookie2":[[[8]],{"len":1}]}]"#,
                "_dd.appsec.s.req.headers": r#"[{"accept":[[[8]],{"len":1}],"content-length":[[[8]],{"len":1}],"host":[[[8]],{"len":1}],"user-agent":[[[8]],{"len":1}],"x-amzn-trace-id":[[[8]],{"len":1}],"x-datadog-parent-id":[[[8]],{"len":1}],"x-datadog-sampling-priority":[[[8]],{"len":1}],"x-datadog-trace-id":[[[8]],{"len":1}],"x-forwarded-for":[[[8]],{"len":1}],"x-forwarded-port":[[[8]],{"len":1}],"x-forwarded-proto":[[[8]],{"len":1}]}]"#,
                "_dd.appsec.waf.version": waf_version,
                "_dd.origin": "appsec",
                "appsec.event": "true",
                "http.method": "POST",
                "http.request.headers.accept": "*/*",
                "http.request.headers.content-length": "19",
                "http.request.headers.host": "x02yirxc7a.execute-api.sa-east-1.amazonaws.com",
                "http.request.headers.user-agent": "Arachni/v2",
                "http.request.headers.x-amzn-trace-id": "Root=1-613a52fb-4c43cfc95e0241c1471bfa05",
                "http.request.headers.x-forwarded-for": "38.122.226.210",
                "http.route": "POST /httpapi/post",
                "http.url": "x02yirxc7a.execute-api.sa-east-1.amazonaws.com/httpapi/post",
                "http.status_code": "200",
                "network.client.ip": "38.122.226.210",
            }
            metrics {
                "_dd.appsec.enabled": 1.0,
                "_dd.appsec.waf.duration": ANY_POSITIVE_VALUE_IS_FINE,
                "_dd.appsec.waf.timeouts": 0.0,
                "_sampling_priority_v1": 2.0,
            }
        },
        span,
    );
}

#[track_caller]
fn assert_span_matches(expected: Span, actual: Span) {
    for (key, value) in &expected.meta {
        if key.starts_with("_dd.appsec.s.") || key == "_dd.appsec.json" {
            // The schemas are JSON objects that may have different key orders...
            let expected_value = serde_json::from_str::<serde_json::Value>(value)
                .expect("failed to parse expected value as JSON");
            let actual_value = serde_json::from_str::<serde_json::Value>(
                actual
                    .meta
                    .get(key)
                    .unwrap_or_else(|| panic!("missing span tag {key}")),
            )
            .expect("failed to parse actual value as JSON");
            assert_eq!(
                expected_value, actual_value,
                "mismatched span tag value for {key}={actual_value} (expected {expected_value})"
            );
        } else {
            assert_eq!(
                value,
                actual
                    .meta
                    .get(key)
                    .unwrap_or_else(|| panic!("missing span tag {key}")),
                "mismatched span tag value for {key}"
            );
        }
    }
    for (key, value) in &expected.metrics {
        if value.is_nan() {
            assert!(
                actual
                    .metrics
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| panic!("missing span metric {key}"))
                    > 0.0,
                "missing required span span metric {key} (any positive value is fine)"
            );
        } else {
            assert_eq!(
                *value,
                actual
                    .metrics
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| panic!("missing span metric {key}")),
                "mismatched value for span metric {key}"
            );
        }
    }

    assert_eq!(
        expected.meta.keys().sorted().collect::<Vec<_>>(),
        actual.meta.keys().sorted().collect::<Vec<_>>(),
        "mismatched number of span tags"
    );
    assert_eq!(
        expected.metrics.keys().sorted().collect::<Vec<_>>(),
        actual.metrics.keys().sorted().collect::<Vec<_>>(),
        "mismatched number of span metrics"
    );
}
