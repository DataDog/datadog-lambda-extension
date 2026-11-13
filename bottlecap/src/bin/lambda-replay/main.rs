//! `lambda-replay`: record a function, replay a recording offline, or serve
//! a recording to in-process tests.
//!
//! ```text
//! lambda-replay replay --recording DIR [--update | --passthrough] [--rules FILE]
//!                      [--report FILE] [common]
//! lambda-replay record --events FILE --out DIR [--wrapper PATH] [--function-name NAME] [common]
//! lambda-replay serve  --recording DIR [--update] [--rules FILE]
//! common: [--env K=V]... [--extension PATH | --no-extension] [--handler H]
//!         [--timeout SECS] [--verbose] [-- RUNTIME_CMD ARGS...]
//! ```
//!
//! `replay` plays the Lambda control plane and every upstream the function
//! called, runs the real runtime (and bottlecap), and reports where the
//! function diverged from the recording. `--update` sends calls the recording
//! lacks to the real upstream and rewrites the recording with what this run
//! did, so an intentional change costs only its new calls.
//!
//! `record` plays the Lambda control plane with real network access and runs
//! bottlecap in record mode with the runtime behind `datadog_wrapper`.
//!
//! `serve` runs only the replay proxy, with a control API, for test helpers
//! that call the handler in-process (see `local_tests/replay/python`).
//!
//! Rules (see `bottlecap::replay::rules`) come from `--rules`, or
//! `rules.json` in the recording directory.

mod control_plane;
mod report;
mod sandbox;
mod serve;

use bottlecap::replay::ca::CertificateAuthority;
use bottlecap::replay::format::{Body, Headers, Record, ResultKind};
use bottlecap::replay::mitm;
use bottlecap::replay::player::{self, InvocationRun, Outcome, Player};
use bottlecap::replay::rules::Rules;
use report::{InvocationReport, SessionReport, Unmatched};
use sandbox::{Invocation, SandboxConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const DEFAULT_EXTENSION: &str = "/opt/extensions/datadog-agent";
const DEFAULT_WRAPPER: &str = "/opt/datadog_wrapper";
const DEFAULT_RUNTIME: &str = "/var/runtime/bootstrap";

/// Host variables every spawned process keeps; the rest of the environment
/// comes from the recording or `--env`.
const HOST_ENV: &[&str] = &["PATH", "HOME", "TMPDIR", "LANG", "USER", "LD_LIBRARY_PATH"];

/// Recorded variables that would point the replay back at Lambda, re-enable
/// recording, or override the host's.
pub const DROPPED_ENV: &[&str] = &[
    "AWS_LAMBDA_RUNTIME_API",
    "AWS_LAMBDA_EXEC_WRAPPER",
    "DD_REPLAY_RECORD",
    "DD_REPLAY_S3_BUCKET",
    "DD_REPLAY_S3_PREFIX",
    "HOSTNAME",
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "USER",
    "LD_LIBRARY_PATH",
    "LAMBDA_TASK_ROOT",
    "LAMBDA_RUNTIME_DIR",
    "PYTHONPATH",
    "NODE_PATH",
];

/// AWS credential variables passed through from the host when calls go upstream.
const AWS_HOST_ENV: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
];

/// Credentials for a replay that never reaches AWS: SDKs sign with them, the
/// replay ignores signatures.
pub const FAKE_CREDENTIALS: &[(&str, &str)] = &[
    ("AWS_ACCESS_KEY_ID", "AKIAREPLAYREPLAYREPL"),
    ("AWS_SECRET_ACCESS_KEY", "lambda-replay"),
    ("AWS_SESSION_TOKEN", "lambda-replay"),
];

enum Mode {
    Replay {
        recording: PathBuf,
        passthrough: bool,
        update: bool,
        report: Option<PathBuf>,
    },
    Record {
        events: PathBuf,
        out: PathBuf,
        wrapper: PathBuf,
        function_name: String,
    },
    Serve {
        recording: PathBuf,
        update: bool,
    },
}

struct Args {
    mode: Mode,
    /// Function environment (`record`) or overrides of the recorded one (`replay`).
    env: BTreeMap<String, String>,
    rules: Option<PathBuf>,
    extension: Option<PathBuf>,
    handler: Option<String>,
    timeout: Duration,
    verbose: bool,
    runtime_cmd: Vec<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  lambda-replay replay --recording DIR [--update | --passthrough] [--rules FILE] [--report FILE] [common]\n  \
         lambda-replay record --events FILE --out DIR [--wrapper PATH] [--function-name NAME] [common]\n  \
         lambda-replay serve  --recording DIR [--update] [--rules FILE]\n\
         common: [--env K=V]... [--extension PATH | --no-extension] [--handler H] [--timeout SECS]\n\
         \x20       [--verbose] [-- RUNTIME_CMD ARGS...]"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let sub = it.next().unwrap_or_else(|| usage());
    let mut recording = None;
    let mut passthrough = false;
    let mut update = false;
    let mut report = None;
    let mut rules = None;
    let mut events = None;
    let mut out = None;
    let mut wrapper = PathBuf::from(DEFAULT_WRAPPER);
    let mut function_name = "lambda-replay-function".to_string();
    let mut env = BTreeMap::new();
    let mut extension = Some(PathBuf::from(DEFAULT_EXTENSION));
    let mut explicit_extension = false;
    let mut handler = None;
    let mut timeout = Duration::from_secs(30);
    let mut verbose = false;
    let mut runtime_cmd = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--recording" => recording = it.next().map(PathBuf::from),
            "--passthrough" => passthrough = true,
            "--update" => update = true,
            "--report" => report = it.next().map(PathBuf::from),
            "--rules" => rules = it.next().map(PathBuf::from),
            "--events" => events = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "--wrapper" => wrapper = it.next().map_or_else(|| usage(), PathBuf::from),
            "--function-name" => function_name = it.next().unwrap_or_else(|| usage()),
            "--env" => {
                let kv = it.next().unwrap_or_else(|| usage());
                let (k, v) = kv.split_once('=').unwrap_or_else(|| usage());
                env.insert(k.to_string(), v.to_string());
            }
            "--extension" => {
                extension = it.next().map(PathBuf::from);
                explicit_extension = true;
            }
            "--no-extension" => extension = None,
            "--handler" => handler = it.next(),
            "--timeout" => {
                timeout = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .map_or_else(|| usage(), Duration::from_secs);
            }
            "--verbose" | "-v" => verbose = true,
            "--" => runtime_cmd = it.by_ref().collect(),
            _ => usage(),
        }
    }
    if !explicit_extension && extension.as_ref().is_some_and(|p| !p.exists()) {
        extension = None;
    }
    if runtime_cmd.is_empty() {
        runtime_cmd = vec![DEFAULT_RUNTIME.to_string()];
    }
    let mode = match sub.as_str() {
        "replay" => Mode::Replay {
            recording: recording.unwrap_or_else(|| usage()),
            passthrough,
            update,
            report,
        },
        "record" => Mode::Record {
            events: events.unwrap_or_else(|| usage()),
            out: out.unwrap_or_else(|| usage()),
            wrapper,
            function_name,
        },
        "serve" => Mode::Serve {
            recording: recording.unwrap_or_else(|| usage()),
            update,
        },
        _ => usage(),
    };
    Args {
        mode,
        env,
        rules,
        extension,
        handler,
        timeout,
        verbose,
        runtime_cmd,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = parse_args();
    let ok = match &args.mode {
        Mode::Replay {
            recording,
            passthrough,
            update,
            report,
        } => replay(&args, recording, *passthrough, *update, report.as_ref()).await?,
        Mode::Record {
            events,
            out,
            wrapper,
            function_name,
        } => record(&args, events, out, wrapper, function_name).await?,
        Mode::Serve { recording, update } => {
            let rules = load_rules(args.rules.as_deref(), recording)?;
            serve::run(recording, rules, *update).await?;
            true
        }
    };
    std::process::exit(i32::from(!ok));
}

fn load_rules(explicit: Option<&Path>, recording: &Path) -> anyhow::Result<Rules> {
    let path = explicit.map_or_else(|| recording.join("rules.json"), Path::to_path_buf);
    Rules::load(&path).map_err(|e| anyhow::anyhow!(e))
}

fn host_env() -> BTreeMap<String, String> {
    HOST_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v)))
        .collect()
}

pub fn aws_host_env() -> BTreeMap<String, String> {
    AWS_HOST_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v)))
        .collect()
}

fn passthrough_client() -> anyhow::Result<reqwest::Client> {
    Ok(
        datadog_fips::reqwest_adapter::create_reqwest_client_builder()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
    )
}

/// Environment for a process whose outbound HTTP(S) goes through the replay
/// proxy at `proxy`, trusting the CA files in `ca_dir`.
pub fn proxy_env(proxy: &str, ca_dir: &Path) -> BTreeMap<String, String> {
    let bundle = ca_dir.join("ca-bundle.pem").to_string_lossy().into_owned();
    let ca_only = ca_dir.join("ca.pem").to_string_lossy().into_owned();
    [
        ("HTTP_PROXY", proxy),
        ("HTTPS_PROXY", proxy),
        ("http_proxy", proxy),
        ("https_proxy", proxy),
        ("NO_PROXY", "127.0.0.1,localhost"),
        ("no_proxy", "127.0.0.1,localhost"),
        ("NODE_USE_ENV_PROXY", "1"),
        ("AWS_CA_BUNDLE", bundle.as_str()),
        ("REQUESTS_CA_BUNDLE", bundle.as_str()),
        ("SSL_CERT_FILE", bundle.as_str()),
        ("NODE_EXTRA_CA_CERTS", ca_only.as_str()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Environment bottlecap needs to flush to the control plane's fake intake.
fn extension_env(base: &BTreeMap<String, String>, cp_url: &str) -> BTreeMap<String, String> {
    let mut env = base.clone();
    let intake = format!("http://{cp_url}");
    for k in [
        "DD_DD_URL",
        "DD_URL",
        "DD_APM_DD_URL",
        "DD_LOGS_CONFIG_LOGS_DD_URL",
    ] {
        env.insert(k.into(), intake.clone());
    }
    env.insert("DD_API_KEY".into(), "lambda-replay".into());
    env.insert("DD_HTTP_PROTOCOL".into(), "http1".into());
    env.insert("DD_PROXY_HTTPS".into(), "http://127.0.0.1:1".into());
    env.insert("DD_SERVERLESS_FLUSH_STRATEGY".into(), "end".into());
    env
}

// ---------------------------------------------------------------- replay

async fn replay(
    args: &Args,
    recording: &Path,
    passthrough: bool,
    update: bool,
    report_path: Option<&PathBuf>,
) -> anyhow::Result<bool> {
    let rules = load_rules(args.rules.as_deref(), recording)?;
    let sessions = bottlecap::replay::format::read_sessions(recording)?;
    if sessions.is_empty() {
        anyhow::bail!("no sessions in {}", recording.display());
    }
    let mut reports = Vec::new();
    for session in sessions {
        let (report, updated) = replay_session(
            args,
            &session.records,
            rules.clone(),
            passthrough || update,
            update,
        )
        .await?;
        if let Some(records) = updated {
            bottlecap::replay::format::write_session(&session.dir, &records)?;
            println!("updated {}", session.dir.display());
        }
        reports.push(report);
    }
    let passed = if update {
        report::print_update(&reports)
    } else {
        report::print(&reports)
    };
    if let Some(path) = report_path {
        std::fs::write(path, serde_json::to_vec_pretty(&reports)?)?;
    }
    Ok(passed)
}

fn recorded_invocations(records: &[Record]) -> Vec<(Invocation, Option<(ResultKind, Body)>)> {
    let mut out: Vec<(Invocation, Option<(ResultKind, Body)>)> = Vec::new();
    for r in records {
        match r {
            Record::Invocation {
                request_id,
                headers,
                event,
                ..
            } => out.push((
                Invocation {
                    request_id: request_id.clone(),
                    headers: headers.clone(),
                    event: event.to_bytes(),
                },
                None,
            )),
            Record::Result {
                request_id,
                kind,
                body,
                ..
            } => {
                if let Some((_, expected)) =
                    out.iter_mut().find(|(i, _)| &i.request_id == request_id)
                {
                    *expected = Some((*kind, body.clone()));
                }
            }
            _ => {}
        }
    }
    out
}

/// The `Session` record and its environment minus what must not carry over.
pub fn session_of(records: &[Record]) -> (Record, BTreeMap<String, String>) {
    let session = records
        .iter()
        .find(|r| matches!(r, Record::Session { .. }))
        .cloned()
        .unwrap_or_else(|| Record::Session {
            version: bottlecap::replay::format::FORMAT_VERSION,
            sandbox_id: "fixture".into(),
            function_name: "function".into(),
            region: "us-east-1".into(),
            runtime: String::new(),
            env: BTreeMap::new(),
        });
    let env = match &session {
        Record::Session { env, .. } => env
            .iter()
            .filter(|(k, _)| !DROPPED_ENV.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        _ => BTreeMap::new(),
    };
    (session, env)
}

#[allow(clippy::too_many_lines)]
async fn replay_session(
    args: &Args,
    records: &[Record],
    rules: Rules,
    forward_unmatched: bool,
    update: bool,
) -> anyhow::Result<(SessionReport, Option<Vec<Record>>)> {
    let (session, recorded_env) = session_of(records);
    let Record::Session {
        sandbox_id,
        function_name,
        ..
    } = &session
    else {
        unreachable!("session_of returns a Session");
    };
    let mut report = SessionReport::new(sandbox_id);
    let handler = args
        .handler
        .clone()
        .or_else(|| match &session {
            Record::Session { env, .. } => env.get("_HANDLER").cloned(),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("no _HANDLER in the recording; pass --handler"))?;

    let work = tempfile::tempdir()?;
    let ca = Arc::new(CertificateAuthority::generate()?);
    ca.write_files(work.path())?;

    let player = Player::new(
        records,
        rules,
        if forward_unmatched {
            Some(passthrough_client()?)
        } else {
            None
        },
    );
    let mitm_listener = TcpListener::bind("127.0.0.1:0").await?;
    let mitm_addr = mitm_listener.local_addr()?;
    let shutdown = CancellationToken::new();
    tokio::spawn(mitm::serve(
        mitm_listener,
        Arc::clone(&ca),
        player.handler(),
        shutdown.clone(),
    ));

    let mut base = host_env();
    base.extend(recorded_env);
    base.insert("AWS_LAMBDA_INITIALIZATION_TYPE".into(), "on-demand".into());

    let mut runtime_env = base.clone();
    runtime_env.insert("_HANDLER".into(), handler.clone());
    runtime_env.extend(proxy_env(&format!("http://{mitm_addr}"), work.path()));
    if forward_unmatched {
        // Forwarded calls must be signed with real credentials.
        runtime_env.extend(aws_host_env());
    } else {
        for (k, v) in FAKE_CREDENTIALS {
            runtime_env.insert((*k).to_string(), (*v).to_string());
        }
    }
    runtime_env.extend(args.env.clone());

    let recorded = recorded_invocations(records);
    let config = SandboxConfig {
        function_name: function_name.clone(),
        handler,
        extension: args.extension.clone(),
        extension_env: base,
        runtime_cmd: args.runtime_cmd.clone(),
        runtime_env,
        timeout: args.timeout,
        verbose: args.verbose,
        current: Arc::clone(&player.current),
    };
    let invocations: Vec<Invocation> = recorded.iter().map(|(i, _)| i.clone()).collect();
    let outcome = sandbox::run(config, invocations).await?;
    shutdown.cancel();

    report.errors = outcome.errors;
    let ignored = player.response_ignore();
    let mut runs = Vec::new();
    for (result, (inv, expected)) in outcome.invocations.into_iter().zip(&recorded) {
        let mut ir = InvocationReport::new(&result.request_id);
        ir.errors = result.errors;
        match (&result.result, expected) {
            (None, _) => ir
                .errors
                .push(format!("no result within {:?}", args.timeout)),
            (Some(actual), Some((kind, body))) => {
                ir.compare_result(*kind, &body.to_bytes(), actual, &ignored);
            }
            (Some(_), None) => ir
                .errors
                .push("recording has no result for this invocation".into()),
        }
        let id = Some(result.request_id.as_str());
        let log = player.log_for(id);
        ir.served = log
            .iter()
            .filter(|l| matches!(l.outcome, Outcome::Served { .. }))
            .count();
        let unmatched: Vec<Unmatched> = log
            .iter()
            .filter(|l| !matches!(l.outcome, Outcome::Served { .. }))
            .map(Into::into)
            .collect();
        if update {
            ir.added = unmatched;
        } else {
            ir.unmatched = unmatched;
        }
        ir.missing = player.remaining(id).iter().map(Into::into).collect();
        runs.push(InvocationRun {
            request_id: result.request_id.clone(),
            headers: inv.headers.clone(),
            event: Body::from_bytes(&inv.event),
            result: result
                .result
                .as_ref()
                .map(|r| (r.kind, Body::from_bytes(&r.body))),
        });
        report.invocations.push(ir);
    }
    report.init_missing = player.remaining(None).iter().map(Into::into).collect();
    let init_unmatched: Vec<Unmatched> = player
        .log_for(None)
        .iter()
        .filter(|l| !matches!(l.outcome, Outcome::Served { .. }))
        .map(Into::into)
        .collect();
    if update {
        report.init_added = init_unmatched;
    } else {
        report.init_unmatched = init_unmatched;
    }
    report.intake_requests = outcome.intake.len();

    let updated = (update && report.errors.is_empty())
        .then(|| player::updated_session(session.clone(), records, &player.log(), &runs));
    Ok((report, updated))
}

// ---------------------------------------------------------------- record

fn read_events(path: &PathBuf) -> anyhow::Result<Vec<Value>> {
    let data = std::fs::read_to_string(path)?;
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&data) {
        return Ok(items);
    }
    data.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(Into::into))
        .collect()
}

async fn record(
    args: &Args,
    events_path: &PathBuf,
    out: &PathBuf,
    wrapper: &Path,
    function_name: &str,
) -> anyhow::Result<bool> {
    let Some(extension) = args.extension.clone() else {
        anyhow::bail!("record needs the extension: pass --extension PATH");
    };
    let handler = args
        .handler
        .clone()
        .ok_or_else(|| anyhow::anyhow!("record needs --handler"))?;
    let events = read_events(events_path)?;
    let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into());
    let started = SystemTime::now();

    let mut base = host_env();
    base.extend(aws_host_env());
    for (k, v) in [
        ("AWS_LAMBDA_FUNCTION_NAME", function_name),
        ("AWS_LAMBDA_FUNCTION_VERSION", "$LATEST"),
        ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "128"),
        ("AWS_LAMBDA_INITIALIZATION_TYPE", "on-demand"),
        ("AWS_REGION", region.as_str()),
        ("AWS_DEFAULT_REGION", region.as_str()),
        ("_HANDLER", handler.as_str()),
    ] {
        base.insert(k.into(), v.into());
    }
    base.extend(args.env.clone());
    base.insert(
        "AWS_LAMBDA_EXEC_WRAPPER".into(),
        DEFAULT_WRAPPER.to_string(),
    );
    base.insert("DD_REPLAY_RECORD".into(), "true".into());

    let mut runtime_cmd = vec![wrapper.to_string_lossy().into_owned()];
    runtime_cmd.extend(args.runtime_cmd.clone());

    let stamp = started
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let invocations = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let request_id = format!("{stamp:08x}-0000-4000-8000-{i:012x}");
            Invocation {
                headers: synthetic_headers(&request_id, &region, function_name, i),
                request_id,
                event: serde_json::to_vec(e).unwrap_or_default(),
            }
        })
        .collect();

    let config = SandboxConfig {
        function_name: function_name.to_string(),
        handler,
        extension: Some(extension),
        extension_env: base.clone(),
        runtime_cmd,
        runtime_env: base,
        timeout: args.timeout,
        verbose: args.verbose,
        current: Arc::default(),
    };
    // The wrapper points the runtime at bottlecap's proxy, which forwards to
    // the control plane.
    let outcome = sandbox::run(config, invocations).await?;

    let mut ok = outcome.errors.is_empty();
    for e in &outcome.errors {
        println!("! {e}");
    }
    for inv in &outcome.invocations {
        let body = inv
            .result
            .as_ref()
            .map(|r| String::from_utf8_lossy(&r.body).into_owned());
        println!(
            "{}  {:?}  {}",
            inv.request_id,
            inv.result.as_ref().map(|r| r.kind),
            body.unwrap_or_else(|| "<no result>".into())
        );
        for e in &inv.errors {
            println!("  ! {e}");
        }
        ok &= inv.result.is_some() && inv.errors.is_empty();
    }

    let copied = copy_new_recordings(started, out)?;
    if copied == 0 {
        println!("! bottlecap wrote no recording segments");
        ok = false;
    } else {
        println!("wrote {copied} segment(s) to {}", out.display());
    }
    Ok(ok)
}

/// `/next` headers for an invocation that has no recorded ones.
pub fn synthetic_headers(request_id: &str, region: &str, function_name: &str, i: usize) -> Headers {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    [
        ("lambda-runtime-aws-request-id", request_id.to_string()),
        (
            "lambda-runtime-invoked-function-arn",
            format!("arn:aws:lambda:{region}:123456789012:function:{function_name}"),
        ),
        (
            "lambda-runtime-trace-id",
            format!("Root=1-{stamp:08x}-{i:024x};Parent={i:016x};Sampled=1"),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), vec![v]))
    .collect()
}

/// Copies sandbox directories bottlecap wrote since `since` into `out`.
fn copy_new_recordings(since: SystemTime, out: &Path) -> std::io::Result<usize> {
    let src = PathBuf::from(bottlecap::replay::REPLAY_DIR).join("recordings");
    let mut copied = 0;
    let Ok(entries) = std::fs::read_dir(&src) else {
        return Ok(0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since);
        if !path.is_dir() || !fresh {
            continue;
        }
        let dest = out.join(entry.file_name());
        std::fs::create_dir_all(&dest)?;
        for f in std::fs::read_dir(&path)?.flatten() {
            std::fs::copy(f.path(), dest.join(f.file_name()))?;
            copied += 1;
        }
    }
    Ok(copied)
}
