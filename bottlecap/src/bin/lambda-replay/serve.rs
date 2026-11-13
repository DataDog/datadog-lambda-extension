//! `lambda-replay serve`: the replay proxy alone, for tests that call the
//! handler in-process. Prints one JSON line on stdout with the proxy address,
//! CA files, control API and recorded invocations, then serves until
//! `POST /shutdown`.
//!
//! Control API (JSON bodies):
//! - `POST /begin {"request_id", "event"?}`: start an invocation. A recorded
//!   one gets its recorded calls back; an unknown one needs `event`.
//! - `POST /end {"request_id", "kind"?, "body"?}`: finish it and get
//!   `{served, unmatched, forwarded, missing}`.
//! - `POST /shutdown`: with `--update`, rewrite the recording first.

use crate::report::{Missing, Unmatched};
use axum::{Json, Router, extract::State, routing::post};
use bottlecap::replay::ca::CertificateAuthority;
use bottlecap::replay::format::{Body, Headers, Record, ResultKind};
use bottlecap::replay::mitm;
use bottlecap::replay::player::{self, InvocationRun, Outcome, Player};
use bottlecap::replay::rules::Rules;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

struct ServeState {
    player: Arc<Player>,
    records: Vec<Record>,
    session: Record,
    dir: PathBuf,
    update: bool,
    region: String,
    function_name: String,
    runs: Mutex<Vec<InvocationRun>>,
    shutdown: CancellationToken,
}

#[derive(Deserialize)]
struct Begin {
    request_id: String,
    #[serde(default)]
    event: Option<Value>,
}

#[derive(Deserialize)]
struct End {
    request_id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    body: Option<Value>,
}

fn body_json(b: &Body) -> Value {
    b.text
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_else(|| Value::String(b.text.clone().unwrap_or_default()))
}

fn recorded(records: &[Record], id: &str) -> Option<(Headers, Body)> {
    records.iter().find_map(|r| match r {
        Record::Invocation {
            request_id,
            headers,
            event,
            ..
        } if request_id == id => Some((headers.clone(), event.clone())),
        _ => None,
    })
}

pub async fn run(recording: &Path, rules: Rules, update: bool) -> anyhow::Result<()> {
    let sessions = bottlecap::replay::format::read_sessions(recording)?;
    let (dir, records) = sessions.into_iter().next().map_or_else(
        || (recording.join("fixture"), Vec::new()),
        |s| (s.dir, s.records),
    );
    let (session, env) = crate::session_of(&records);
    let (function_name, region) = match &session {
        Record::Session {
            function_name,
            region,
            ..
        } => (function_name.clone(), region.clone()),
        _ => (String::new(), String::new()),
    };

    let work = tempfile::tempdir()?;
    let ca = Arc::new(CertificateAuthority::generate()?);
    ca.write_files(work.path())?;
    let player = Player::new(
        &records,
        rules,
        if update {
            Some(crate::passthrough_client()?)
        } else {
            None
        },
    );
    let mitm_listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy = format!("http://{}", mitm_listener.local_addr()?);
    let shutdown = CancellationToken::new();
    tokio::spawn(mitm::serve(
        mitm_listener,
        ca,
        player.handler(),
        shutdown.clone(),
    ));

    let invocations: Vec<Value> = records
        .iter()
        .filter_map(|r| match r {
            Record::Invocation {
                request_id, event, ..
            } => Some(json!({"request_id": request_id, "event": body_json(event)})),
            _ => None,
        })
        .collect();
    let state = Arc::new(ServeState {
        player,
        records,
        session,
        dir,
        update,
        region,
        function_name,
        runs: Mutex::default(),
        shutdown: shutdown.clone(),
    });
    let control = TcpListener::bind("127.0.0.1:0").await?;
    let control_url = format!("http://{}", control.local_addr()?);
    let router = Router::new()
        .route("/begin", post(begin))
        .route("/end", post(end))
        .route("/shutdown", post(stop))
        .with_state(state);
    let server_shutdown = shutdown.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(control, router)
            .with_graceful_shutdown(async move { server_shutdown.cancelled().await })
            .await;
    });

    let mut function_env = env;
    if !update {
        for (k, v) in crate::FAKE_CREDENTIALS {
            function_env.insert((*k).to_string(), (*v).to_string());
        }
    }
    function_env.extend(crate::proxy_env(&proxy, work.path()));
    println!(
        "{}",
        json!({
            "proxy": proxy,
            "control": control_url,
            "update": update,
            "env": function_env,
            "invocations": invocations,
        })
    );
    let _ = server.await;
    Ok(())
}

async fn begin(State(s): State<Arc<ServeState>>, Json(b): Json<Begin>) -> Json<Value> {
    let (headers, event) = match (recorded(&s.records, &b.request_id), b.event) {
        (Some((headers, _)), Some(e)) => (headers, Body::from_bytes(e.to_string().as_bytes())),
        (Some((headers, event)), None) => (headers, event),
        (None, e) => {
            let n = s.runs.lock().map_or(0, |r| r.len());
            (
                crate::synthetic_headers(&b.request_id, &s.region, &s.function_name, n),
                Body::from_bytes(e.unwrap_or(Value::Null).to_string().as_bytes()),
            )
        }
    };
    s.player.begin(&b.request_id);
    if let Ok(mut runs) = s.runs.lock() {
        runs.retain(|r| r.request_id != b.request_id);
        runs.push(InvocationRun {
            request_id: b.request_id.clone(),
            headers: headers.clone(),
            event: event.clone(),
            result: None,
        });
    }
    Json(json!({"request_id": b.request_id, "event": body_json(&event), "headers": headers}))
}

async fn end(State(s): State<Arc<ServeState>>, Json(e): Json<End>) -> Json<Value> {
    s.player.set_current(None);
    let kind = match e.kind.as_deref() {
        Some("response") => Some(ResultKind::Response),
        Some("error") => Some(ResultKind::Error),
        _ => None,
    };
    if let (Ok(mut runs), Some(kind)) = (s.runs.lock(), kind)
        && let Some(run) = runs.iter_mut().find(|r| r.request_id == e.request_id)
    {
        let body = e.body.unwrap_or(Value::Null).to_string();
        run.result = Some((kind, Body::from_bytes(body.as_bytes())));
    }
    let log = s.player.log_for(Some(&e.request_id));
    let pick = |want: Outcome| -> Vec<Unmatched> {
        log.iter()
            .filter(|l| l.outcome == want)
            .map(Into::into)
            .collect()
    };
    let missing: Vec<Missing> = s
        .player
        .remaining(Some(&e.request_id))
        .iter()
        .map(Into::into)
        .collect();
    Json(json!({
        "served": log.iter().filter(|l| matches!(l.outcome, Outcome::Served { .. })).count(),
        "unmatched": pick(Outcome::Unmatched),
        "forwarded": pick(Outcome::Forwarded),
        "missing": missing,
    }))
}

async fn stop(State(s): State<Arc<ServeState>>) -> Json<Value> {
    let mut written = Value::Null;
    if s.update {
        let runs = s.runs.lock().map(|r| r.clone()).unwrap_or_default();
        let records =
            player::updated_session(s.session.clone(), &s.records, &s.player.log(), &runs);
        match bottlecap::replay::format::write_session(&s.dir, &records) {
            Ok(()) => written = json!(s.dir.display().to_string()),
            Err(e) => written = json!(format!("error: {e}")),
        }
    }
    s.shutdown.cancel();
    Json(json!({"written": written}))
}
