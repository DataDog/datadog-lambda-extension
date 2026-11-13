//! Collects records for one sandbox and writes a segment per invocation.

use crate::replay::format::{
    Body, Headers, HttpRequest, HttpResponse, Record, ResultKind, headers_from, to_jsonl,
};
use crate::replay::s3::S3Uploader;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::{debug, error};

#[derive(Default)]
struct State {
    seq: u64,
    /// The invocation in flight; `None` during init and between invocations.
    current: Option<String>,
    pending: Vec<Record>,
    segment: u32,
    runtime_captured: bool,
}

pub struct Recorder {
    sandbox_id: String,
    function_name: String,
    dir: PathBuf,
    /// Written by `datadog_wrapper` with what only the runtime sees (`_HANDLER`, ...).
    runtime_env_file: Option<PathBuf>,
    uploader: Option<S3Uploader>,
    state: Mutex<State>,
    uploads: tokio::sync::Mutex<JoinSet<()>>,
}

impl Recorder {
    #[must_use]
    pub fn new(
        dir: PathBuf,
        sandbox_id: String,
        session: Record,
        function_name: String,
        runtime_env_file: Option<PathBuf>,
        uploader: Option<S3Uploader>,
    ) -> Self {
        Self {
            sandbox_id,
            function_name,
            dir,
            runtime_env_file,
            uploader,
            state: Mutex::new(State {
                pending: vec![session],
                ..State::default()
            }),
            uploads: tokio::sync::Mutex::new(JoinSet::new()),
        }
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> Option<T> {
        match self.state.lock() {
            Ok(mut s) => Some(f(&mut s)),
            Err(e) => {
                error!("REPLAY | recorder state poisoned: {e}");
                None
            }
        }
    }

    /// Called when `/next` returns, before the runtime sees the event, so
    /// outbound calls the handler makes are attributed to this invocation.
    pub fn begin_invocation(&self, request_id: &str, headers: &http::HeaderMap) {
        let headers: Headers = headers_from(headers, true)
            .into_iter()
            .filter(|(k, _)| k.starts_with("lambda-runtime-"))
            .collect();
        let runtime_env = self.read_runtime_env();
        self.with_state(|s| {
            if !s.runtime_captured
                && let Some(vars) = runtime_env
            {
                for r in &mut s.pending {
                    if let Record::Session { env, runtime, .. } = r {
                        if let Some(rt) = vars.get("AWS_EXECUTION_ENV") {
                            runtime.clone_from(rt);
                        }
                        env.extend(vars.clone());
                    }
                }
                s.runtime_captured = true;
            }
            s.seq += 1;
            s.current = Some(request_id.to_string());
            s.pending.push(Record::Invocation {
                seq: s.seq,
                request_id: request_id.to_string(),
                headers,
                event: Body::default(),
            });
        });
    }

    fn read_runtime_env(&self) -> Option<std::collections::BTreeMap<String, String>> {
        let data = std::fs::read_to_string(self.runtime_env_file.as_ref()?).ok()?;
        Some(
            data.lines()
                .filter_map(|l| l.split_once('='))
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    /// Fills in the event body once the tee has read it.
    pub fn set_event(&self, request_id: &str, body: &[u8]) {
        self.with_state(|s| {
            for r in s.pending.iter_mut().rev() {
                if let Record::Invocation {
                    request_id: id,
                    event,
                    ..
                } = r
                    && id == request_id
                {
                    *event = Body::from_bytes(body);
                    return;
                }
            }
            debug!("REPLAY | event for {request_id} arrived after its segment was written");
        });
    }

    pub fn on_exchange(
        &self,
        mut request: HttpRequest,
        response: HttpResponse,
        duration: Duration,
    ) {
        crate::replay::format::redact(&mut request.headers);
        self.with_state(|s| {
            s.seq += 1;
            s.pending.push(Record::Exchange {
                seq: s.seq,
                request_id: s.current.clone(),
                request,
                response,
                duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            });
        });
    }

    pub async fn on_result(
        &self,
        request_id: &str,
        kind: ResultKind,
        headers: &http::HeaderMap,
        body: &[u8],
    ) {
        let headers: Headers = headers_from(headers, true)
            .into_iter()
            .filter(|(k, _)| k.starts_with("lambda-runtime-"))
            .collect();
        self.with_state(|s| {
            s.seq += 1;
            s.pending.push(Record::Result {
                seq: s.seq,
                request_id: request_id.to_string(),
                kind,
                headers,
                body: Body::from_bytes(body),
            });
            s.current = None;
        });
        self.flush().await;
    }

    /// Writes pending records as the next segment and starts its upload.
    pub async fn flush(&self) {
        let Some((records, segment)) = self.with_state(|s| {
            s.segment += 1;
            (std::mem::take(&mut s.pending), s.segment)
        }) else {
            return;
        };
        if records.is_empty() {
            return;
        }
        let data = to_jsonl(&records);
        let name = format!("{segment:05}.jsonl");
        let dir = self.dir.join(&self.sandbox_id);
        if let Err(e) =
            std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join(&name), &data))
        {
            error!("REPLAY | failed to write segment {name}: {e}");
        }
        if let Some(uploader) = self.uploader.clone() {
            let key = format!("{}/{}/{name}", self.function_name, self.sandbox_id);
            self.uploads.lock().await.spawn(async move {
                match uploader.put(&key, data).await {
                    Ok(()) => debug!("REPLAY | uploaded s3://{}/{key}", uploader.bucket),
                    Err(e) => error!("REPLAY | upload of {key} failed: {e}"),
                }
            });
        }
    }

    /// Waits up to `budget` for segment uploads, so the sandbox is not frozen mid-upload.
    pub async fn wait_uploads(&self, budget: Duration) {
        let mut uploads = self.uploads.lock().await;
        let _ = tokio::time::timeout(budget, async {
            while uploads.join_next().await.is_some() {}
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::format::read_dir;

    fn req() -> HttpRequest {
        HttpRequest {
            method: "GET".into(),
            url: "https://api.example.com/".into(),
            headers: [("authorization".to_string(), vec!["secret".to_string()])].into(),
            body: Body::default(),
        }
    }

    fn resp() -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: Headers::new(),
            body: Body::from_bytes(b"ok"),
        }
    }

    #[tokio::test]
    async fn attributes_exchanges_and_writes_segments() {
        let tmp = tempfile::tempdir().expect("tmp");
        let session = Record::Session {
            version: 1,
            sandbox_id: "sb".into(),
            function_name: "f".into(),
            region: "us-east-1".into(),
            runtime: "python3.12".into(),
            env: std::collections::BTreeMap::default(),
        };
        let runtime_env = tmp.path().join("runtime.env");
        std::fs::write(
            &runtime_env,
            "_HANDLER=app.handler\nAWS_EXECUTION_ENV=AWS_Lambda_python3.12\n",
        )
        .expect("write");
        let r = Recorder::new(
            tmp.path().join("rec"),
            "sb".into(),
            session,
            "f".into(),
            Some(runtime_env),
            None,
        );
        r.on_exchange(req(), resp(), Duration::from_millis(3));
        r.begin_invocation("r1", &http::HeaderMap::new());
        r.set_event("r1", b"{\"k\":1}");
        r.on_exchange(req(), resp(), Duration::from_millis(3));
        r.on_result(
            "r1",
            ResultKind::Response,
            &http::HeaderMap::new(),
            b"\"done\"",
        )
        .await;

        let records = read_dir(&tmp.path().join("rec")).expect("read");
        assert!(matches!(&records[0], Record::Session { runtime, env, .. }
            if runtime == "AWS_Lambda_python3.12" && env.get("_HANDLER").map(String::as_str) == Some("app.handler")));
        let ids: Vec<Option<String>> = records
            .iter()
            .filter_map(|r| match r {
                Record::Exchange {
                    request_id,
                    request,
                    ..
                } => {
                    assert!(!request.headers.contains_key("authorization"));
                    Some(request_id.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![None, Some("r1".to_string())]);
        assert!(records.iter().any(|r| matches!(r, Record::Invocation { event, .. } if event.text.as_deref() == Some("{\"k\":1}"))));
    }
}
