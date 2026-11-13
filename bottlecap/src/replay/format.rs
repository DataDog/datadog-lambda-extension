//! Recording format: one JSON record per line, segments ordered by file name.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;

/// Headers never written to a recording.
const DROPPED_HEADERS: &[&str] = &[
    "authorization",
    "x-amz-security-token",
    "cookie",
    "set-cookie",
    "proxy-authorization",
];

/// A body as UTF-8 text when valid, base64 otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Body {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
}

impl Body {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(s) => Self {
                text: Some(s.to_string()),
                base64: None,
            },
            Err(_) => Self {
                text: None,
                base64: Some(B64.encode(bytes)),
            },
        }
    }

    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        if let Some(t) = &self.text {
            return t.as_bytes().to_vec();
        }
        self.base64
            .as_ref()
            .and_then(|b| B64.decode(b).ok())
            .unwrap_or_default()
    }
}

/// Lowercased header names to values. Repeated headers keep every value.
pub type Headers = BTreeMap<String, Vec<String>>;

/// Drops credential headers before a request is written to a recording.
pub fn redact(headers: &mut Headers) {
    for h in DROPPED_HEADERS {
        headers.remove(*h);
    }
}

#[must_use]
pub fn headers_from(map: &http::HeaderMap, redact: bool) -> Headers {
    let mut out = Headers::new();
    for (name, value) in map {
        let name = name.as_str().to_ascii_lowercase();
        if redact && DROPPED_HEADERS.contains(&name.as_str()) {
            continue;
        }
        out.entry(name)
            .or_default()
            .push(String::from_utf8_lossy(value.as_bytes()).into_owned());
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    /// Absolute URL, `https://host[:port]/path?query`.
    pub url: String,
    pub headers: Headers,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Response,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Session {
        version: u32,
        sandbox_id: String,
        function_name: String,
        region: String,
        runtime: String,
        /// Environment the function saw, minus credentials.
        env: BTreeMap<String, String>,
    },
    /// An event the runtime received from `/next`.
    Invocation {
        seq: u64,
        request_id: String,
        /// `Lambda-Runtime-*` headers from `/next`.
        headers: Headers,
        event: Body,
    },
    /// An outbound HTTP exchange. `request_id` is `None` during init or
    /// between invocations.
    Exchange {
        seq: u64,
        request_id: Option<String>,
        request: HttpRequest,
        response: HttpResponse,
        duration_ms: u64,
    },
    /// What the runtime posted to `/response` or `/error`.
    Result {
        seq: u64,
        request_id: String,
        kind: ResultKind,
        headers: Headers,
        body: Body,
    },
    /// Spans the tracer sent to the extension during an invocation, as JSON.
    Spans {
        request_id: String,
        spans: Vec<serde_json::Value>,
    },
}

#[must_use]
pub fn to_jsonl(records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in records {
        if let Ok(line) = serde_json::to_vec(r) {
            out.extend_from_slice(&line);
            out.push(b'\n');
        }
    }
    out
}

pub fn parse_jsonl(data: &str) -> Result<Vec<Record>, serde_json::Error> {
    data.lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// Reads every `*.jsonl` segment under `dir` (recursively), in path order.
pub fn read_dir(dir: &Path) -> std::io::Result<Vec<Record>> {
    let mut files = Vec::new();
    collect_jsonl(dir, &mut files)?;
    files.sort();
    let mut records = Vec::new();
    for f in files {
        let data = std::fs::read_to_string(&f)?;
        let mut parsed = parse_jsonl(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", f.display()),
            )
        })?;
        records.append(&mut parsed);
    }
    Ok(records)
}

/// One sandbox's recording: the directory its segments live in, and its records.
pub struct SessionFiles {
    pub dir: std::path::PathBuf,
    pub records: Vec<Record>,
}

/// Reads a recording grouped by the directory each segment is in (one per sandbox).
pub fn read_sessions(dir: &Path) -> std::io::Result<Vec<SessionFiles>> {
    let mut files = Vec::new();
    if dir.exists() {
        collect_jsonl(dir, &mut files)?;
    }
    files.sort();
    let mut sessions: Vec<SessionFiles> = Vec::new();
    for f in files {
        let parent = f.parent().unwrap_or(dir).to_path_buf();
        let data = std::fs::read_to_string(&f)?;
        let mut parsed = parse_jsonl(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", f.display()),
            )
        })?;
        match sessions.last_mut() {
            Some(s) if s.dir == parent => s.records.append(&mut parsed),
            _ => sessions.push(SessionFiles {
                dir: parent,
                records: parsed,
            }),
        }
    }
    Ok(sessions)
}

/// Replaces a sandbox's segments with one segment holding `records`.
pub fn write_session(dir: &Path, records: &[Record]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            std::fs::remove_file(path)?;
        }
    }
    std::fs::write(dir.join("00001.jsonl"), to_jsonl(records))
}

fn collect_jsonl(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_round_trips_text_and_binary() {
        let text = Body::from_bytes(b"{\"a\":1}");
        assert_eq!(text.text.as_deref(), Some("{\"a\":1}"));
        assert_eq!(text.to_bytes(), b"{\"a\":1}");

        let bin = Body::from_bytes(&[0xff, 0x00, 0x10]);
        assert!(bin.text.is_none());
        assert_eq!(bin.to_bytes(), vec![0xff, 0x00, 0x10]);
    }

    #[test]
    fn headers_drop_credentials() {
        let mut map = http::HeaderMap::new();
        map.insert("Authorization", "AWS4-HMAC-SHA256 ...".parse().expect("hv"));
        map.insert(
            "X-Amz-Target",
            "DynamoDB_20120810.GetItem".parse().expect("hv"),
        );
        let h = headers_from(&map, true);
        assert!(!h.contains_key("authorization"));
        assert_eq!(h["x-amz-target"], vec!["DynamoDB_20120810.GetItem"]);
    }

    #[test]
    fn jsonl_round_trip() {
        let records = vec![
            Record::Invocation {
                seq: 1,
                request_id: "r1".into(),
                headers: Headers::new(),
                event: Body::from_bytes(b"{}"),
            },
            Record::Result {
                seq: 2,
                request_id: "r1".into(),
                kind: ResultKind::Response,
                headers: Headers::new(),
                body: Body::from_bytes(b"\"ok\""),
            },
        ];
        let bytes = to_jsonl(&records);
        let parsed = parse_jsonl(std::str::from_utf8(&bytes).expect("utf8")).expect("parse");
        assert_eq!(parsed, records);
    }
}
