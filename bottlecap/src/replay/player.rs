//! Replay state shared by `lambda-replay replay` and `lambda-replay serve`:
//! answers the function's outbound requests from a recording, logs what
//! happened, and rebuilds the recording for `--update`.

use crate::replay::format::{Body, Headers, HttpRequest, HttpResponse, Record, ResultKind, redact};
use crate::replay::matcher::{Entry, Matcher, NearMiss};
use crate::replay::mitm::{self, Handler};
use crate::replay::rules::Rules;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Answered from recorded exchange `seq`.
    Served { seq: u64 },
    /// Not in the recording; sent to the real upstream (`--update`, `--passthrough`).
    Forwarded,
    /// Not in the recording; answered with an error.
    Unmatched,
}

#[derive(Clone, Debug)]
pub struct Logged {
    pub request_id: Option<String>,
    pub request: HttpRequest,
    pub response: HttpResponse,
    pub outcome: Outcome,
    pub near_miss: Option<NearMiss>,
    pub duration_ms: u64,
}

pub struct Player {
    matcher: Mutex<Matcher>,
    /// The invocation in flight; `None` during init and between invocations.
    pub current: Arc<Mutex<Option<String>>>,
    log: Mutex<Vec<Logged>>,
    passthrough: Option<reqwest::Client>,
    rules: Rules,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Player {
    /// `passthrough` sends unmatched requests upstream instead of failing them.
    #[must_use]
    pub fn new(
        records: &[Record],
        rules: Rules,
        passthrough: Option<reqwest::Client>,
    ) -> Arc<Self> {
        Arc::new(Self {
            matcher: Mutex::new(Matcher::new(records, rules.clone())),
            current: Arc::default(),
            log: Mutex::default(),
            passthrough,
            rules,
        })
    }

    pub fn set_current(&self, request_id: Option<String>) {
        *lock(&self.current) = request_id;
    }

    /// Starts (or restarts) an invocation: its recorded exchanges become
    /// available again and its earlier log entries are discarded.
    pub fn begin(&self, request_id: &str) {
        lock(&self.matcher).reset(Some(request_id));
        lock(&self.log).retain(|l| l.request_id.as_deref() != Some(request_id));
        self.set_current(Some(request_id.to_string()));
    }

    #[must_use]
    pub fn handler(self: &Arc<Self>) -> Handler {
        let player = Arc::clone(self);
        Arc::new(move |req| {
            let player = Arc::clone(&player);
            Box::pin(async move { player.handle(req).await })
        })
    }

    async fn handle(&self, req: HttpRequest) -> HttpResponse {
        let started = Instant::now();
        let request_id = lock(&self.current).clone();
        let taken = lock(&self.matcher).take(request_id.as_deref(), &req);
        let (response, outcome, near_miss) = if let Some(entry) = taken {
            (entry.response, Outcome::Served { seq: entry.seq }, None)
        } else {
            let near_miss = lock(&self.matcher).near_miss(request_id.as_deref(), &req);
            match &self.passthrough {
                Some(client) => (
                    mitm::forward(client, &req).await,
                    Outcome::Forwarded,
                    near_miss,
                ),
                None => (
                    unmatched_response(&req.method, &req.url),
                    Outcome::Unmatched,
                    near_miss,
                ),
            }
        };
        lock(&self.log).push(Logged {
            request_id,
            request: req,
            response: response.clone(),
            outcome,
            near_miss,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        response
    }

    #[must_use]
    pub fn log(&self) -> Vec<Logged> {
        lock(&self.log).clone()
    }

    /// Logged exchanges of one invocation (`None`: init and between invocations).
    #[must_use]
    pub fn log_for(&self, request_id: Option<&str>) -> Vec<Logged> {
        lock(&self.log)
            .iter()
            .filter(|l| l.request_id.as_deref() == request_id)
            .cloned()
            .collect()
    }

    /// Recorded exchanges of `request_id` the function never requested.
    #[must_use]
    pub fn remaining(&self, request_id: Option<&str>) -> Vec<Entry> {
        lock(&self.matcher).remaining(request_id)
    }

    #[must_use]
    pub fn response_ignore(&self) -> Vec<&str> {
        self.rules
            .ignore_response
            .iter()
            .map(String::as_str)
            .collect()
    }
}

/// A 400 with an AWS JSON error body: SDKs surface it without retrying.
#[must_use]
pub fn unmatched_response(method: &str, url: &str) -> HttpResponse {
    let body = serde_json::json!({
        "__type": "LambdaReplayUnmatchedRequest",
        "message": format!("lambda-replay: no recorded response for {method} {url}"),
    });
    HttpResponse {
        status: 400,
        headers: [
            (
                "content-type".to_string(),
                vec!["application/x-amz-json-1.0".to_string()],
            ),
            (
                "x-amzn-errortype".to_string(),
                vec!["LambdaReplayUnmatchedRequest".to_string()],
            ),
        ]
        .into(),
        body: Body::from_bytes(body.to_string().as_bytes()),
    }
}

/// What an invocation looked like in this run, for rebuilding the recording.
#[derive(Clone, Debug)]
pub struct InvocationRun {
    pub request_id: String,
    pub headers: Headers,
    pub event: Body,
    /// `None` keeps the recorded result.
    pub result: Option<(ResultKind, Body)>,
}

/// Rebuilds a session from a run: invocations in `runs` get the exchanges and
/// results this run produced, the init pool gets this run's init exchanges,
/// and invocations the run did not touch are kept as recorded. Unused
/// recorded exchanges of touched invocations are dropped.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn updated_session(
    session: Record,
    original: &[Record],
    log: &[Logged],
    runs: &[InvocationRun],
) -> Vec<Record> {
    let touched: HashSet<&str> = runs.iter().map(|r| r.request_id.as_str()).collect();
    let mut out = vec![session];
    let mut seq = 0u64;
    let mut next = || {
        seq += 1;
        seq
    };
    let exchange = |l: &Logged, seq: u64| {
        let mut request = l.request.clone();
        redact(&mut request.headers);
        Record::Exchange {
            seq,
            request_id: l.request_id.clone(),
            request,
            response: l.response.clone(),
            duration_ms: l.duration_ms,
        }
    };

    for l in log
        .iter()
        .filter(|l| l.request_id.is_none() && l.outcome != Outcome::Unmatched)
    {
        out.push(exchange(l, next()));
    }

    let mut order: Vec<String> = Vec::new();
    for r in original {
        if let Record::Invocation { request_id, .. } = r
            && !order.contains(request_id)
        {
            order.push(request_id.clone());
        }
    }
    for r in runs {
        if !order.contains(&r.request_id) {
            order.push(r.request_id.clone());
        }
    }

    for id in order {
        if let Some(run) = runs.iter().find(|r| r.request_id == id) {
            out.push(Record::Invocation {
                seq: next(),
                request_id: id.clone(),
                headers: run.headers.clone(),
                event: run.event.clone(),
            });
            for l in log.iter().filter(|l| {
                l.request_id.as_deref() == Some(id.as_str()) && l.outcome != Outcome::Unmatched
            }) {
                out.push(exchange(l, next()));
            }
            let recorded = original.iter().find_map(|r| match r {
                Record::Result {
                    request_id,
                    kind,
                    body,
                    headers,
                    ..
                } if *request_id == id => Some((*kind, body.clone(), headers.clone())),
                _ => None,
            });
            let result = run
                .result
                .clone()
                .map(|(k, b)| (k, b, Headers::new()))
                .or(recorded);
            if let Some((kind, body, headers)) = result {
                out.push(Record::Result {
                    seq: next(),
                    request_id: id.clone(),
                    kind,
                    headers,
                    body,
                });
            }
        } else if !touched.contains(id.as_str()) {
            for r in original {
                let mut r = r.clone();
                let keep = match &mut r {
                    Record::Invocation {
                        request_id, seq: s, ..
                    }
                    | Record::Result {
                        request_id, seq: s, ..
                    } => {
                        *s = next();
                        *request_id == id
                    }
                    Record::Exchange {
                        request_id,
                        seq: s,
                        request,
                        ..
                    } => {
                        *s = next();
                        redact(&mut request.headers);
                        request_id.as_deref() == Some(id.as_str())
                    }
                    Record::Spans { request_id, .. } => *request_id == id,
                    Record::Session { .. } => false,
                };
                if keep {
                    out.push(r);
                }
            }
        }
    }
    // Untouched records took sequence numbers in a loop over every record; renumber densely.
    let mut n = 0u64;
    for r in &mut out {
        if let Record::Invocation { seq, .. }
        | Record::Exchange { seq, .. }
        | Record::Result { seq, .. } = r
        {
            n += 1;
            *seq = n;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str, body: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".into(),
            url: url.into(),
            headers: [("authorization".to_string(), vec!["secret".to_string()])].into(),
            body: Body::from_bytes(body.as_bytes()),
        }
    }

    fn resp(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: Headers::new(),
            body: Body::from_bytes(body.as_bytes()),
        }
    }

    fn session() -> Record {
        Record::Session {
            version: 1,
            sandbox_id: "sb".into(),
            function_name: "f".into(),
            region: "us-east-1".into(),
            runtime: String::new(),
            env: std::collections::BTreeMap::default(),
        }
    }

    fn recording() -> Vec<Record> {
        vec![
            session(),
            Record::Invocation {
                seq: 1,
                request_id: "r1".into(),
                headers: Headers::new(),
                event: Body::from_bytes(b"1"),
            },
            Record::Exchange {
                seq: 2,
                request_id: Some("r1".into()),
                request: req("https://a/old", ""),
                response: resp("old"),
                duration_ms: 1,
            },
            Record::Result {
                seq: 3,
                request_id: "r1".into(),
                kind: ResultKind::Response,
                headers: Headers::new(),
                body: Body::from_bytes(b"\"one\""),
            },
            Record::Invocation {
                seq: 4,
                request_id: "r2".into(),
                headers: Headers::new(),
                event: Body::from_bytes(b"2"),
            },
            Record::Exchange {
                seq: 5,
                request_id: Some("r2".into()),
                request: req("https://a/two", ""),
                response: resp("two"),
                duration_ms: 1,
            },
            Record::Result {
                seq: 6,
                request_id: "r2".into(),
                kind: ResultKind::Response,
                headers: Headers::new(),
                body: Body::from_bytes(b"\"two\""),
            },
        ]
    }

    #[tokio::test]
    async fn serves_logs_and_rebuilds_touched_invocations() {
        let records = recording();
        let player = Player::new(&records, Rules::default(), None);
        player.set_current(Some("r1".into()));
        let served = player.handle(req("https://a/old", "")).await;
        assert_eq!(served.body.text.as_deref(), Some("old"));
        let missed = player.handle(req("https://a/new", "")).await;
        assert_eq!(missed.status, 400);
        assert_eq!(player.log_for(Some("r1")).len(), 2);

        // Pretend `/new` was forwarded (update mode) and r1 now returns something else.
        let mut log = player.log();
        log[1].outcome = Outcome::Forwarded;
        log[1].response = resp("new");
        let runs = vec![InvocationRun {
            request_id: "r1".into(),
            headers: Headers::new(),
            event: Body::from_bytes(b"1"),
            result: Some((ResultKind::Response, Body::from_bytes(b"\"changed\""))),
        }];
        let updated = updated_session(session(), &records, &log, &runs);
        let urls: Vec<(String, Option<String>)> = updated
            .iter()
            .filter_map(|r| match r {
                Record::Exchange {
                    request,
                    request_id,
                    ..
                } => {
                    assert!(!request.headers.contains_key("authorization"));
                    Some((request.url.clone(), request_id.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            urls,
            vec![
                ("https://a/old".to_string(), Some("r1".to_string())),
                ("https://a/new".to_string(), Some("r1".to_string())),
                ("https://a/two".to_string(), Some("r2".to_string())),
            ]
        );
        let results: Vec<Option<String>> = updated
            .iter()
            .filter_map(|r| match r {
                Record::Result { body, .. } => Some(body.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            results,
            vec![Some("\"changed\"".into()), Some("\"two\"".into())]
        );
        let seqs: Vec<u64> = updated
            .iter()
            .filter_map(|r| match r {
                Record::Invocation { seq, .. }
                | Record::Exchange { seq, .. }
                | Record::Result { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, (1..=7).collect::<Vec<_>>());
    }
}
