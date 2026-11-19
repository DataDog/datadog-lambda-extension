// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "test-mode")]
#![allow(clippy::pedantic, clippy::unwrap_used, clippy::expect_used)]

//! The real Bottlecap binary and published Datadog Node provider, with local fakes.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use harness::prelude::*;
use httpmock::{Method::GET, MockServer};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
};

const PATH: &str = "/api/v2/feature-flagging/config/rules-based/server";

struct NodeApp {
    child: Child,
    input: ChildStdin,
    output: Receiver<String>,
}

impl NodeApp {
    fn start() -> Self {
        let example =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/feature-flag-config-cache");
        assert!(
            example
                .join("node_modules/@datadog/openfeature-node-server/package.json")
                .exists(),
            "install the pinned SDK with npm ci in {}",
            example.display()
        );
        let mut child = Command::new("node")
            .arg("app.mjs")
            .current_dir(example)
            .env("CONFIG_CACHE_URL", "http://127.0.0.1:8126")
            .env("DD_ENV", "prod")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start real Datadog SDK application");
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.expect("SDK output line")).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output,
        }
    }

    fn invoke(&mut self, subject: &str) -> Value {
        writeln!(self.input, "{}", json!({"targetingKey":subject})).unwrap();
        self.input.flush().unwrap();
        let line = self
            .output
            .recv_timeout(secs(5))
            .expect("real SDK response by deadline");
        let value: Value = serde_json::from_str(&line).expect("SDK JSON response");
        assert!(value.get("error").is_none(), "{value}");
        assert_eq!(value["sdk"]["name"], "@datadog/openfeature-node-server");
        assert_eq!(value["sdk"]["version"], "2.7.0");
        assert!(
            value["sdk"]["resolved"]
                .as_str()
                .unwrap()
                .contains("node_modules/@datadog/openfeature-node-server/")
        );
        value
    }
}

impl Drop for NodeApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn configuration(version: u8) -> Value {
    json!({"data":{"attributes":{
        "createdAt":format!("2026-11-17T00:00:{version:02}.000Z"),
        "format":"SERVER", "environment":{"name":"prod"},
        "flags":{"kill-switch":{
            "key":"kill-switch","enabled":true,"variationType":"BOOLEAN",
            "variations":{"selected":{"key":"selected","value":version >= 2}},
            "allocations":[{"key":"experiment-allocation","rules":[],
                "splits":[{"variationKey":"selected","shards":[]}],"doLog":true}]
        }}
    }}})
}

fn evaluated(result: &Value, expected: bool) {
    assert_eq!(result["value"], expected, "{result}");
    assert_eq!(result["variant"], "selected", "{result}");
    assert!(result.get("errorCode").is_none(), "{result}");
}

async fn begin(
    s: &Scenario,
    bc: &mut BottlecapProcess,
    id: &str,
    poll: usize,
    cold: bool,
) -> Instant {
    let deadline = s
        .invoke(id, secs(30), poll, bc.startup_deadline() + secs(30))
        .await
        .unwrap_or_else(|| panic!("no /next. {}", bc.log_tail(40)));
    deliver_start(&s.api, id, cold).await.unwrap();
    deadline
}

async fn end(s: &Scenario, bc: &BottlecapProcess, id: &str, poll: usize, deadline: Instant) {
    deliver_runtime_done(&s.api, id, true).await.unwrap();
    assert!(
        s.api.wait_for_next_calls(poll, deadline).await,
        "no /next. {}",
        bc.log_tail(40)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(bottlecap_ports)]
async fn real_sdk_invocations_use_the_bottlecap_cache() {
    for enabled in [false, true] {
        let cdn = MockServer::start_async().await;
        let rules = cdn
            .mock_async(|when, then| {
                when.method(GET)
                    .path(PATH)
                    .header("DD-API-KEY", TEST_API_KEY)
                    .query_param("dd_env", "prod");
                then.status(200)
                    .header("ETag", "v2")
                    .json_body(configuration(2));
            })
            .await;
        let s = Scenario::start(FaultMode::Ok).await;
        let mut bc = s
            .bottlecap()
            .env("DD_TEST_FLAG_CONFIG_BASE_URL", &cdn.base_url())
            .env(
                "DD_SERVERLESS_FLAG_CONFIG_CACHE_ENABLED",
                if enabled { "true" } else { "false" },
            )
            .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_TIMEOUT", "1")
            .spawn();
        let mut app = NodeApp::start();
        for i in 0..100 {
            let id = format!("invocation-{i}");
            let deadline = begin(&s, &mut bc, &id, i + 1, i == 0).await;
            let result = app.invoke("subject-a");
            evaluated(&result, true);
            assert_eq!(
                result["exposures"].as_array().unwrap().len(),
                usize::from(i == 0)
            );
            end(&s, &bc, &id, i + 2, deadline).await;
        }
        let deadline = begin(&s, &mut bc, "warm", 101, false).await;
        evaluated(&app.invoke("subject-a"), true);
        end(&s, &bc, "warm", 102, deadline).await;
        // Count requests only after asserting real configured values, SDK identity, and key authentication.
        assert_eq!(
            rules.hits_async().await,
            if enabled { 1 } else { 101 },
            "{}",
            bc.log_tail(40)
        );
        drop(app);
        for i in 0..4 {
            let id = format!("fresh-sdk-{i}");
            let deadline = begin(&s, &mut bc, &id, 102 + i, false).await;
            let mut app = NodeApp::start();
            let result = app.invoke("new-sdk-instance");
            evaluated(&result, true);
            assert_eq!(result["exposures"].as_array().unwrap().len(), 1);
            end(&s, &bc, &id, 103 + i, deadline).await;
        }
        assert_eq!(rules.hits_async().await, if enabled { 1 } else { 105 });
        s.shutdown_cleanly(&mut bc).await;
        println!(
            "Bottlecap cache={enabled}: 105 real SDK evaluations; CDN={}; five SDK processes",
            rules.hits_async().await
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(bottlecap_ports)]
async fn real_sdk_refresh_stale_expiry_and_recovery() {
    let cdn = MockServer::start_async().await;
    let rules = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY);
            then.status(200)
                .header("ETag", "v1")
                .json_body(configuration(1));
        })
        .await;
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s
        .bottlecap()
        .env("DD_TEST_FLAG_CONFIG_BASE_URL", &cdn.base_url())
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_ENABLED", "true")
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_TTL", "0")
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_MAX_AGE", "1")
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_TIMEOUT", "1")
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_FAILURE_BACKOFF", "0")
        .spawn();
    let deadline = begin(&s, &mut bc, "refresh", 1, true).await;
    let mut app = NodeApp::start();
    evaluated(&app.invoke("subject-a"), false);
    rules.delete_async().await;
    let rules = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY)
                .header("If-None-Match", "v1");
            then.status(200)
                .header("ETag", "v2")
                .json_body(configuration(2));
        })
        .await;
    let updated = app.invoke("subject-a");
    evaluated(&updated, true);
    assert_eq!(updated["exposures"].as_array().unwrap().len(), 1);
    rules.assert_async().await;
    rules.delete_async().await;
    let unavailable = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY);
            then.status(503);
        })
        .await;
    let stale = app.invoke("subject-a");
    evaluated(&stale, true);
    assert_eq!(stale["cacheSource"], "stale");
    assert!(stale["exposures"].as_array().unwrap().is_empty());
    let expiry_deadline = Instant::now() + secs(3);
    loop {
        let result = app.invoke("subject-a");
        if result.get("errorCode").is_some() {
            assert_eq!(result["value"], false);
            assert!(result["exposures"].as_array().unwrap().is_empty());
            break;
        }
        assert!(
            Instant::now() < expiry_deadline,
            "expired rules remained usable: {result}; {}",
            bc.log_tail(40)
        );
        tokio::task::yield_now().await;
    }
    unavailable.delete_async().await;
    let recovered = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY)
                .header("If-None-Match", "v2");
            then.status(304);
        })
        .await;
    evaluated(&app.invoke("subject-a"), true);
    recovered.assert_async().await;
    recovered.delete_async().await;
    let revoked = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY);
            then.status(403);
        })
        .await;
    let rejected = app.invoke("subject-a");
    assert_eq!(rejected["value"], false);
    assert!(rejected.get("errorCode").is_some());
    revoked.assert_async().await;
    end(&s, &bc, "refresh", 2, deadline).await;
    s.shutdown_cleanly(&mut bc).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(bottlecap_ports)]
async fn bottlecap_coalesces_reads_and_returns_local_304() {
    let cdn = MockServer::start_async().await;
    let rules = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY);
            then.status(200)
                .header("ETag", "v2")
                .delay(ms(60))
                .json_body(configuration(2));
        })
        .await;
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s
        .bottlecap()
        .env("DD_TEST_FLAG_CONFIG_BASE_URL", &cdn.base_url())
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_ENABLED", "true")
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_TIMEOUT", "1")
        .spawn();
    let deadline = begin(&s, &mut bc, "concurrent", 1, true).await;
    let client = reqwest::Client::new();
    let results = futures::future::join_all(
        (0..32).map(|_| client.get(format!("http://127.0.0.1:8126{PATH}")).send()),
    )
    .await;
    for response in results {
        let response = response.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.json::<Value>().await.unwrap(), configuration(2));
    }
    assert_eq!(rules.hits_async().await, 1);
    let unchanged = client
        .get(format!("http://127.0.0.1:8126{PATH}"))
        .header("If-None-Match", "v2")
        .send()
        .await
        .unwrap();
    assert_eq!(unchanged.status(), 304);
    assert_eq!(rules.hits_async().await, 1);
    end(&s, &bc, "concurrent", 2, deadline).await;
    s.shutdown_cleanly(&mut bc).await;
    rules.delete_async().await;
}

async fn pause(pid: u32) {
    assert!(
        Command::new("kill")
            .args(["-STOP", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + secs(2);
    loop {
        let output = Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&output.stdout).contains('T') {
            return;
        }
        assert!(Instant::now() < deadline, "process {pid} did not stop");
        tokio::time::sleep(ms(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(bottlecap_ports)]
async fn actual_bottlecap_and_sdk_retain_state_across_process_suspension() {
    let cdn = MockServer::start_async().await;
    let rules = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY);
            then.status(200)
                .header("ETag", "v2")
                .json_body(configuration(2));
        })
        .await;
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s
        .bottlecap()
        .env("DD_TEST_FLAG_CONFIG_BASE_URL", &cdn.base_url())
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_ENABLED", "true")
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_TTL", "0")
        .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_TIMEOUT", "1")
        .spawn();
    let deadline = begin(&s, &mut bc, "before-pause", 1, true).await;
    let mut app = NodeApp::start();
    evaluated(&app.invoke("subject-a"), true);
    rules.assert_async().await;
    end(&s, &bc, "before-pause", 2, deadline).await;
    pause(app.child.id()).await;
    pause(bc.pid).await;
    rules.delete_async().await;
    let unchanged = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY)
                .header("If-None-Match", "v2");
            then.status(304);
        })
        .await;
    assert_eq!(unchanged.hits_async().await, 0);
    for pid in [bc.pid, app.child.id()] {
        assert!(
            Command::new("kill")
                .args(["-CONT", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
    }
    let deadline = begin(&s, &mut bc, "after-pause", 2, false).await;
    let resumed = app.invoke("subject-a");
    evaluated(&resumed, true); // A 304 can only produce true if Bottlecap retained the full snapshot.
    assert!(resumed["exposures"].as_array().unwrap().is_empty());
    unchanged.assert_async().await;
    end(&s, &bc, "after-pause", 3, deadline).await;
    s.shutdown_cleanly(&mut bc).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(bottlecap_ports)]
async fn fresh_bottlecap_environments_each_need_a_bootstrap_fetch() {
    let cdn = MockServer::start_async().await;
    let rules = cdn
        .mock_async(|when, then| {
            when.method(GET)
                .path(PATH)
                .header("DD-API-KEY", TEST_API_KEY);
            then.status(200)
                .header("ETag", "v2")
                .json_body(configuration(2));
        })
        .await;
    for expected_fetches in 1..=3 {
        let s = Scenario::start(FaultMode::Ok).await;
        let mut bc = s
            .bottlecap()
            .env("DD_TEST_FLAG_CONFIG_BASE_URL", &cdn.base_url())
            .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_ENABLED", "true")
            .env("DD_SERVERLESS_FLAG_CONFIG_CACHE_TIMEOUT", "1")
            .spawn();
        let deadline = begin(&s, &mut bc, "fresh", 1, true).await;
        let mut app = NodeApp::start();
        evaluated(&app.invoke("subject-a"), true);
        assert_eq!(
            rules.hits_async().await,
            expected_fetches,
            "{}",
            bc.log_tail(40)
        );
        end(&s, &bc, "fresh", 2, deadline).await;
        s.shutdown_cleanly(&mut bc).await;
    }
}
