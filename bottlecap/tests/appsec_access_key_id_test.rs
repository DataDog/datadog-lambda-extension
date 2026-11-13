// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// Integration tests are exempt from the production lint policy in Cargo.toml.
#![allow(
    unreachable_pub,
    clippy::pedantic,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used
)]

//! `DD_APPSEC_AWS_ACCESS_KEY_ID_TAG_ENABLED` through the real binary: extension
//! AppSec on, one Function URL invocation through the Runtime API proxy, from a
//! universally instrumented runtime (as Go and .NET are). Each test asserts what
//! reaches the intake, and that no payload or log line carries the secret or
//! the session token.
//!
//! Every test here spawns the binary, which binds fixed ports, so each one
//! carries `#[serial_test::serial(bottlecap_ports)]` (for `cargo test`) and
//! the binary is in the `fixed-ports` nextest group.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use harness::prelude::*;
use libdd_trace_protobuf::pb::Span;
use serde_json::Value;

const INVOKE_DEADLINE: Duration = secs(10);
const REQUEST_ID: &str = "req-1";
const TRACE_ID: u64 = 4_242_424_242;
const SPAN_ID: u64 = 1_717_171_717;

const TAG: &str = "aws.lambda.access_key_id";
const ACCESS_KEY_ID: &str = "ASIAEXAMPLE000000001";
const SECRET_CANARY: &str = "canarySecretAccessKeyDoNotSend0000000001";
const TOKEN_CANARY: &str = "canarySessionTokenDoNotSend0000000000002";

/// A builder for extension AppSec behind the Datadog wrapper, with fake
/// credentials in the environment.
fn appsec_bottlecap(s: &Scenario) -> BottlecapBuilder<'_> {
    s.bottlecap().envs([
        ("DD_SERVERLESS_APPSEC_ENABLED", "true"),
        ("AWS_LAMBDA_EXEC_WRAPPER", "/opt/datadog_wrapper"),
        ("AWS_ACCESS_KEY_ID", ACCESS_KEY_ID),
        ("AWS_SECRET_ACCESS_KEY", SECRET_CANARY),
        ("AWS_SESSION_TOKEN", TOKEN_CANARY),
    ])
}

fn client() -> reqwest::Client {
    // Test-only client towards the local extension; the FIPS adapter is for production.
    #[allow(clippy::disallowed_methods)]
    reqwest::Client::builder()
        .timeout(secs(5))
        .build()
        .expect("client")
}

/// Plays the function runtime and its tracer for one cold-start invocation:
/// the runtime polls `/next` and posts its response through the extension's
/// Runtime API proxy, and the tracer calls the lifecycle listener around it.
/// Then shuts the extension down cleanly and returns the `aws.lambda` span
/// that reached the intake.
///
/// The span is looked up after SHUTDOWN: with the `end` flush strategy the
/// trace can reach the aggregator after the end-of-invocation flush, and then
/// goes out with the shutdown flush, which the SHUTDOWN deadline bounds.
async fn invoke_then_shutdown(s: &Scenario, bc: &mut BottlecapProcess) -> Span {
    let deadline = s
        .invoke(REQUEST_ID, INVOKE_DEADLINE, 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));
    let event: Value =
        serde_json::from_str(&fixtures::events::payload("lambda_function_url_event"))
            .expect("function url event");
    s.api.push_runtime_event(REQUEST_ID, event);
    deliver_start(&s.api, REQUEST_ID, true).await.unwrap();

    let client = client();
    let proxy = format!("http://127.0.0.1:{PROXY_PORT}/2018-06-01/runtime/invocation");
    let lifecycle = format!("http://127.0.0.1:{LIFECYCLE_PORT}/lambda");

    let next = client
        .get(format!("{proxy}/next"))
        .send()
        .await
        .unwrap_or_else(|e| panic!("runtime /next through the proxy: {e}. {}", bc.log_tail(40)));
    assert_eq!(
        next.headers()
            .get("Lambda-Runtime-Aws-Request-Id")
            .and_then(|v| v.to_str().ok()),
        Some(REQUEST_ID),
        "{}",
        bc.log_tail(40)
    );
    let event = next.bytes().await.expect("runtime /next body");

    let start = client
        .post(format!("{lifecycle}/start-invocation"))
        .header("lambda-runtime-aws-request-id", REQUEST_ID)
        .body(event)
        .send()
        .await
        .expect("start-invocation");
    assert!(
        start.status().is_success(),
        "start-invocation {}",
        start.status()
    );

    let end = client
        .post(format!("{lifecycle}/end-invocation"))
        .header("lambda-runtime-aws-request-id", REQUEST_ID)
        .header("x-datadog-trace-id", TRACE_ID.to_string())
        .header("x-datadog-span-id", SPAN_ID.to_string())
        .header("x-datadog-sampling-priority", "1")
        .body(r#"{"statusCode":200}"#)
        .send()
        .await
        .expect("end-invocation");
    assert!(end.status().is_success(), "end-invocation {}", end.status());

    let response = client
        .post(format!("{proxy}/{REQUEST_ID}/response"))
        .body(r#"{"statusCode":200}"#)
        .send()
        .await
        .expect("runtime response through the proxy");
    assert!(
        response.status().is_success(),
        "runtime response {}",
        response.status()
    );

    // The listener answers end-invocation, and the proxy forwards the event and
    // the response, before the extension processes them. Lambda's Telemetry API
    // delivers platform.runtimeDone after a buffering delay, the fake at once,
    // so wait until the extension holds the invocation end and AppSec has seen
    // both the request and the response. Otherwise runtimeDone can overtake
    // them, and the span goes out late, untagged or not at all.
    wait_for_log(
        bc,
        &format!("Buffered UniversalInstrumentationEnd for request_id: {REQUEST_ID}"),
    )
    .await;
    wait_for_log(
        bc,
        &format!("aap: marking security context for {REQUEST_ID} as having seen the response"),
    )
    .await;
    deliver_runtime_done(&s.api, REQUEST_ID, true)
        .await
        .unwrap();
    assert!(
        s.api.wait_for_next_calls(2, deadline).await,
        "no /next after the invocation by its deadline. {}",
        bc.log_tail(40)
    );
    s.shutdown_cleanly(bc).await;

    let span = s
        .intake
        .requests_to("/api/v0.2/traces")
        .iter()
        .filter(|r| r.api_key() == Some(TEST_API_KEY))
        .filter_map(|r| r.spans().ok())
        .flatten()
        .find(|span| span.name == "aws.lambda" && span.trace_id == TRACE_ID)
        .unwrap_or_else(|| {
            panic!(
                "no aws.lambda span by the SHUTDOWN deadline. intake: {}\n{}",
                s.intake.summary(),
                bc.log_tail(60)
            )
        });
    // AppSec monitored the invocation: the processor wrote its tags.
    assert!(
        span.meta.contains_key("_dd.appsec.waf.version"),
        "AppSec did not process the span: {:?}",
        span.meta
    );
    span
}

/// Wait until the extension logs `needle`. Sequences the fake's next event
/// after internal processing that the extension does not expose otherwise; it
/// asserts nothing about the contract.
async fn wait_for_log(bc: &BottlecapProcess, needle: &str) {
    let deadline = Instant::now() + secs(5);
    while !bc.log_contains(needle) {
        assert!(
            Instant::now() < deadline,
            "no {needle:?} in the log. {}",
            bc.log_tail(40)
        );
        tokio::time::sleep(ms(10)).await;
    }
}

/// No intake request and no line on stdout or stderr carries the secret or the
/// session token.
fn assert_no_canary(s: &Scenario, bc: &BottlecapProcess) {
    for canary in [SECRET_CANARY, TOKEN_CANARY] {
        for r in s.intake.requests() {
            let body = r.decoded_body().unwrap_or_else(|_| r.body.clone());
            assert!(
                !String::from_utf8_lossy(&body).contains(canary),
                "{} {} carries a credential canary",
                r.method,
                r.path
            );
        }
        assert!(
            !bc.stdout().contains(canary) && !bc.stderr().contains(canary),
            "a log line carries a credential canary"
        );
    }
}

/// T11 (S1, S3): the key on tags the invocation span with the environment's
/// access key ID, and neither the secret nor the token leaves the process.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn access_key_id_tagged_and_no_secret_sent() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = appsec_bottlecap(&s)
        .env("DD_APPSEC_AWS_ACCESS_KEY_ID_TAG_ENABLED", "true")
        .spawn();

    let span = invoke_then_shutdown(&s, &mut bc).await;
    assert_eq!(
        span.meta.get(TAG).map(String::as_str),
        Some(ACCESS_KEY_ID),
        "{:?}",
        span.meta
    );

    assert_no_canary(&s, &bc);
    assert!(
        !bc.stdout().contains(ACCESS_KEY_ID) && !bc.stderr().contains(ACCESS_KEY_ID),
        "a log line carries the access key ID"
    );
}

/// T12 (S2): with the key unset, the invocation span carries no tag.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn access_key_id_not_tagged_when_key_unset() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = appsec_bottlecap(&s).spawn();

    let span = invoke_then_shutdown(&s, &mut bc).await;
    assert!(!span.meta.contains_key(TAG), "{:?}", span.meta);

    assert_no_canary(&s, &bc);
}

/// T12 (S2): with the key set to false, the invocation span carries no tag.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn access_key_id_not_tagged_when_key_false() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = appsec_bottlecap(&s)
        .env("DD_APPSEC_AWS_ACCESS_KEY_ID_TAG_ENABLED", "false")
        .spawn();

    let span = invoke_then_shutdown(&s, &mut bc).await;
    assert!(!span.meta.contains_key(TAG), "{:?}", span.meta);

    assert_no_canary(&s, &bc);
}
