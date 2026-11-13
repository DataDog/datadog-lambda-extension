//! Runs one sandbox's lifecycle against the fake control plane: extension
//! init, runtime init, each invocation with its telemetry, then SHUTDOWN.

use crate::control_plane::{ControlPlane, IntakeRequest, InvocationResult, RuntimeEvent};
use bottlecap::replay::format::{Headers, ResultKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct Invocation {
    pub request_id: String,
    pub headers: Headers,
    pub event: Vec<u8>,
}

pub struct SandboxConfig {
    pub function_name: String,
    pub handler: String,
    pub extension: Option<PathBuf>,
    pub extension_env: BTreeMap<String, String>,
    pub runtime_cmd: Vec<String>,
    pub runtime_env: BTreeMap<String, String>,
    pub timeout: Duration,
    pub verbose: bool,
    /// The invocation in flight, for the MITM's attribution.
    pub current: Arc<Mutex<Option<String>>>,
}

pub struct InvocationOutcome {
    pub request_id: String,
    pub result: Option<InvocationResult>,
    pub errors: Vec<String>,
}

pub struct Outcome {
    pub errors: Vec<String>,
    pub invocations: Vec<InvocationOutcome>,
    pub intake: Vec<IntakeRequest>,
}

#[allow(clippy::too_many_lines)]
pub async fn run(config: SandboxConfig, invocations: Vec<Invocation>) -> anyhow::Result<Outcome> {
    let cp = ControlPlane::start(
        config.function_name.clone(),
        config.handler.clone(),
        Arc::clone(&config.current),
    )
    .await?;
    let cp_url = format!("127.0.0.1:{}", cp.addr.port());
    let client = datadog_fips::reqwest_adapter::create_reqwest_client_builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .no_proxy()
        .build()?;
    let mut errors = Vec::new();

    // Extension first: Lambda starts the runtime only after extensions register.
    let started = Instant::now();
    let mut extension = None;
    if let Some(path) = &config.extension {
        let mut env = crate::extension_env(&config.extension_env, &cp_url);
        env.insert("AWS_LAMBDA_RUNTIME_API".into(), cp_url.clone());
        extension = Some(spawn(
            path.to_string_lossy().as_ref(),
            &[],
            &env,
            "extension",
            config.verbose,
        )?);
        if !wait_count(&cp.shared.extension_next_calls, 1, Duration::from_secs(10)).await {
            errors.push("extension did not call /next within 10s".into());
        }
    }

    let mut runtime_env = config.runtime_env.clone();
    runtime_env.insert("AWS_LAMBDA_RUNTIME_API".into(), cp_url.clone());
    let mut runtime = spawn(
        &config.runtime_cmd[0],
        &config.runtime_cmd[1..],
        &runtime_env,
        "runtime",
        true,
    )?;

    let init_ok = tokio::select! {
        ok = wait_count(&cp.shared.runtime_next_calls, 1, Duration::from_secs(30)) => {
            if !ok {
                errors.push("runtime did not call /next within 30s".into());
            }
            ok
        }
        status = runtime.wait() => {
            errors.push(format!("runtime exited during init: {status:?}"));
            false
        }
    };
    if let Some(e) = cp.shared.init_error.lock().ok().and_then(|g| g.clone()) {
        errors.push(format!("init error: {e}"));
    }
    let init_ms = started.elapsed().as_secs_f64() * 1000.0;
    let init_status = if init_ok { "success" } else { "error" };
    send_telemetry(
        &client,
        &cp,
        &[
            tel(
                "platform.initStart",
                json!({"initializationType": "on-demand", "phase": "init",
                       "runtimeVersion": "replay",
                       "runtimeVersionArn": "arn:aws:lambda:us-east-1::runtime:replay"}),
            ),
            tel(
                "platform.initRuntimeDone",
                json!({"initializationType": "on-demand", "phase": "init", "status": init_status}),
            ),
            tel(
                "platform.initReport",
                json!({"initializationType": "on-demand", "phase": "init", "status": init_status,
                       "metrics": {"durationMs": init_ms}}),
            ),
        ],
    )
    .await;

    let memory: u64 = runtime_env
        .get("AWS_LAMBDA_FUNCTION_MEMORY_SIZE")
        .and_then(|m| m.parse().ok())
        .unwrap_or(128);
    let mut outcomes = Vec::new();
    for (i, inv) in invocations.into_iter().enumerate() {
        if !init_ok {
            break;
        }
        let mut inv_errors = Vec::new();
        let (tx, rx) = oneshot::channel::<InvocationResult>();
        if let Ok(mut r) = cp.shared.results.lock() {
            r.insert(inv.request_id.clone(), tx);
        }
        let deadline_ms =
            epoch_ms() + u64::try_from(config.timeout.as_millis()).unwrap_or(u64::MAX);
        let mut headers = inv.headers.clone();
        headers.insert(
            "lambda-runtime-deadline-ms".into(),
            vec![deadline_ms.to_string()],
        );

        if extension.is_some() {
            let _ = cp.extension_tx.send(json!({
                "eventType": "INVOKE",
                "requestId": inv.request_id,
                "invokedFunctionArn": first(&headers, "lambda-runtime-invoked-function-arn"),
                "deadlineMs": deadline_ms,
                "tracing": {"type": "X-Amzn-Trace-Id", "value": first(&headers, "lambda-runtime-trace-id")},
            }));
        }
        send_telemetry(
            &client,
            &cp,
            &[tel(
                "platform.start",
                json!({"requestId": inv.request_id, "version": "$LATEST"}),
            )],
        )
        .await;
        let invoke_started = Instant::now();
        let _ = cp.runtime_tx.send(RuntimeEvent {
            request_id: inv.request_id.clone(),
            headers,
            body: inv.event.clone(),
        });

        let result = tokio::select! {
            r = tokio::time::timeout(config.timeout, rx) => r.ok().and_then(Result::ok),
            status = runtime.wait() => {
                inv_errors.push(format!("runtime exited: {status:?}"));
                None
            }
        };
        let duration_ms = invoke_started.elapsed().as_secs_f64() * 1000.0;
        let status = match &result {
            Some(r) if r.kind == ResultKind::Response => "success",
            Some(_) => "error",
            None => "timeout",
        };
        send_telemetry(
            &client,
            &cp,
            &[tel(
                "platform.runtimeDone",
                json!({"requestId": inv.request_id, "status": status,
                       "metrics": {"durationMs": duration_ms,
                                   "producedBytes": result.as_ref().map_or(0, |r| r.body.len())}}),
            )],
        )
        .await;
        // The extension calls /next once it has flushed the invocation.
        if extension.is_some()
            && !wait_count(
                &cp.shared.extension_next_calls,
                i + 2,
                Duration::from_secs(10),
            )
            .await
        {
            inv_errors.push("extension did not call /next within 10s".into());
        }
        let mut metrics = json!({"durationMs": duration_ms, "billedDurationMs": duration_ms.ceil(),
                                 "memorySizeMB": memory, "maxMemoryUsedMB": memory / 2});
        if i == 0 {
            metrics["initDurationMs"] = json!(init_ms);
        }
        send_telemetry(
            &client,
            &cp,
            &[tel(
                "platform.report",
                json!({"requestId": inv.request_id, "status": status, "metrics": metrics}),
            )],
        )
        .await;
        outcomes.push(InvocationOutcome {
            request_id: inv.request_id,
            result,
            errors: inv_errors,
        });
    }

    // Like Lambda: stop the runtime, then send SHUTDOWN to extensions.
    let _ = runtime.kill().await;
    if let Some(mut ext) = extension.take() {
        let _ = cp.extension_tx.send(json!({
            "eventType": "SHUTDOWN",
            "shutdownReason": "spindown",
            "deadlineMs": epoch_ms() + 2000,
        }));
        if tokio::time::timeout(Duration::from_secs(3), ext.wait())
            .await
            .is_err()
        {
            errors.push("extension did not exit within the SHUTDOWN deadline".into());
            let _ = ext.kill().await;
        }
    }

    let intake = cp
        .shared
        .intake
        .lock()
        .map(|i| i.clone())
        .unwrap_or_default();
    Ok(Outcome {
        errors,
        invocations: outcomes,
        intake,
    })
}

fn first(headers: &Headers, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.first().cloned())
        .unwrap_or_default()
}

fn spawn(
    program: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    label: &'static str,
    show_output: bool,
) -> anyhow::Result<Child> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start {label} `{program}`: {e}"))?;
    if let Some(out) = child.stdout.take() {
        tokio::spawn(print_lines(out, label, show_output));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(print_lines(err, label, show_output));
    }
    Ok(child)
}

async fn print_lines<R: tokio::io::AsyncRead + Unpin>(r: R, label: &'static str, show: bool) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if show {
            eprintln!("[{label}] {line}");
        }
    }
}

async fn wait_count(w: &tokio::sync::watch::Sender<usize>, n: usize, budget: Duration) -> bool {
    let mut rx = w.subscribe();
    tokio::time::timeout(budget, rx.wait_for(|c| *c >= n))
        .await
        .is_ok_and(|r| r.is_ok())
}

fn tel(kind: &str, record: Value) -> Value {
    json!({
        "time": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "type": kind,
        "record": record,
    })
}

/// Posts to the subscriber like Lambda does, with `sandbox` resolved to localhost.
async fn send_telemetry(client: &reqwest::Client, cp: &ControlPlane, events: &[Value]) {
    let Some(uri) = cp.shared.telemetry_uri.lock().ok().and_then(|u| u.clone()) else {
        return;
    };
    let Ok(mut url) = reqwest::Url::parse(&uri) else {
        return;
    };
    let _ = url.set_host(Some("127.0.0.1"));
    let _ = client
        .post(url)
        .json(events)
        .timeout(Duration::from_secs(2))
        .send()
        .await;
}

fn epoch_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
