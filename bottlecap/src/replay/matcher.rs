//! Matches replayed requests to recorded exchanges.
//!
//! A request's key is its method, normalized URL and normalized body. Headers
//! are ignored: they carry signatures, dates, trace context and SDK invocation
//! IDs that change on every run. Each key holds a FIFO queue of recorded
//! responses, so repeated calls replay in order and concurrent calls can
//! arrive in any order.

use crate::replay::format::{HttpRequest, HttpResponse, Record};
use crate::replay::rules::{self, Rules};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

/// Query parameters that differ on every signed request.
const VOLATILE_QUERY: &[&str] = &[
    "x-amz-date",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "x-amz-expires",
    "x-amz-signedheaders",
    "x-id",
];

/// JSON keys that hold per-call idempotency tokens.
const VOLATILE_JSON_KEYS: &[&str] = &["ClientRequestToken", "ClientToken", "IdempotencyToken"];

#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: u64,
    pub request: HttpRequest,
    pub response: HttpResponse,
}

/// The recorded exchange closest to an unmatched request, for the report.
#[derive(Clone, Debug)]
pub struct NearMiss {
    pub seq: u64,
    pub differences: Vec<String>,
}

#[derive(Default)]
pub struct Matcher {
    /// Keyed by invocation (`None`: init and between invocations), then by request key.
    pools: HashMap<Option<String>, HashMap<String, VecDeque<Entry>>>,
    /// Every recorded exchange, used or not, for near-miss reports.
    all: Vec<(Option<String>, Entry)>,
    rules: Rules,
}

impl Matcher {
    #[must_use]
    pub fn from_records(records: &[Record]) -> Self {
        Self::new(records, Rules::default())
    }

    #[must_use]
    pub fn new(records: &[Record], rules: Rules) -> Self {
        let mut m = Self {
            rules,
            ..Self::default()
        };
        for r in records {
            if let Record::Exchange {
                seq,
                request_id,
                request,
                response,
                ..
            } = r
            {
                m.all.push((
                    request_id.clone(),
                    Entry {
                        seq: *seq,
                        request: request.clone(),
                        response: response.clone(),
                    },
                ));
                let key = m.key(request);
                m.pools
                    .entry(request_id.clone())
                    .or_default()
                    .entry(key)
                    .or_default()
                    .push_back(Entry {
                        seq: *seq,
                        request: request.clone(),
                        response: response.clone(),
                    });
            }
        }
        m
    }

    /// Takes the next recorded response for `req`, first from the
    /// invocation's own exchanges, then from the init/between-invocation pool.
    pub fn take(&mut self, request_id: Option<&str>, req: &HttpRequest) -> Option<Entry> {
        let key = self.key(req);
        let own = request_id.map(ToString::to_string);
        for pool in [own, None] {
            if let Some(entry) = self
                .pools
                .get_mut(&pool)
                .and_then(|p| p.get_mut(&key))
                .and_then(VecDeque::pop_front)
            {
                return Some(entry);
            }
        }
        None
    }

    /// Makes every recorded exchange of `request_id` available again.
    pub fn reset(&mut self, request_id: Option<&str>) {
        let id = request_id.map(ToString::to_string);
        let entries: Vec<Entry> = self
            .all
            .iter()
            .filter(|(i, _)| *i == id)
            .map(|(_, e)| e.clone())
            .collect();
        let mut pool: HashMap<String, VecDeque<Entry>> = HashMap::new();
        for e in entries {
            pool.entry(self.key(&e.request)).or_default().push_back(e);
        }
        self.pools.insert(id, pool);
    }

    /// Recorded exchanges of `request_id` that were never requested.
    #[must_use]
    pub fn remaining(&self, request_id: Option<&str>) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .pools
            .get(&request_id.map(ToString::to_string))
            .map(|p| p.values().flatten().cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|e| e.seq);
        out
    }

    /// The recorded exchange with the same method, URL and operation as
    /// `req` (unused ones first, then the invocation's used ones), and how its
    /// body differs.
    #[must_use]
    pub fn near_miss(&self, request_id: Option<&str>, req: &HttpRequest) -> Option<NearMiss> {
        let target = shape(req);
        let own = request_id.map(ToString::to_string);
        let unused = [own.clone(), None]
            .iter()
            .filter_map(|pool| self.pools.get(pool))
            .flat_map(|p| p.values().flatten())
            .filter(|e| shape(&e.request) == target)
            .min_by_key(|e| e.seq);
        let used = || {
            self.all
                .iter()
                .filter(|(id, e)| (*id == own || id.is_none()) && shape(&e.request) == target)
                .map(|(_, e)| e)
                .min_by_key(|e| e.seq)
        };
        let (ignored, _) = self.rules.for_request(req);
        unused.or_else(used).map(|e| NearMiss {
            seq: e.seq,
            differences: body_differences_ignoring(
                &e.request.body.to_bytes(),
                &req.body.to_bytes(),
                &ignored,
            ),
        })
    }

    /// The request key under this matcher's rules.
    #[must_use]
    pub fn key(&self, req: &HttpRequest) -> String {
        let (body_paths, query) = self.rules.for_request(req);
        let body = normalize_body_ignoring(&req.body.to_bytes(), &body_paths);
        let digest = hex::encode(Sha256::digest(&body));
        format!(
            "{} {} {}",
            req.method.to_ascii_uppercase(),
            normalize_url_ignoring(&req.url, &query),
            &digest[..16]
        )
    }
}

/// Method, normalized URL and AWS operation: what two calls must share to be compared.
fn shape(req: &HttpRequest) -> (String, String, Option<String>) {
    (
        req.method.to_ascii_uppercase(),
        normalize_url(&req.url),
        operation(req),
    )
}

/// The AWS JSON-protocol operation, from `X-Amz-Target` (`DynamoDB_20120810.PutItem` -> `PutItem`).
#[must_use]
pub fn operation(req: &HttpRequest) -> Option<String> {
    req.headers
        .get("x-amz-target")
        .and_then(|v| v.first())
        .map(|t| t.rsplit('.').next().unwrap_or(t).to_string())
}

#[must_use]
pub fn request_key(req: &HttpRequest) -> String {
    Matcher::default().key(req)
}

/// Lowercases scheme and host, drops default ports and volatile query
/// parameters, and sorts the rest.
#[must_use]
pub fn normalize_url(url: &str) -> String {
    normalize_url_ignoring(url, &[])
}

/// [`normalize_url`], also dropping the `ignored` query parameters.
#[must_use]
pub fn normalize_url_ignoring(url: &str, ignored: &[&str]) -> String {
    let Ok(uri) = url.parse::<http::Uri>() else {
        return url.to_string();
    };
    let scheme = uri.scheme_str().unwrap_or("http").to_ascii_lowercase();
    let host = uri.host().unwrap_or_default().to_ascii_lowercase();
    let port = match (scheme.as_str(), uri.port_u16()) {
        ("https", Some(443)) | ("http", Some(80)) | (_, None) => String::new(),
        (_, Some(p)) => format!(":{p}"),
    };
    let mut query: Vec<&str> = uri
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter(|kv| {
            let k = kv.split('=').next().unwrap_or_default();
            !VOLATILE_QUERY.contains(&k.to_ascii_lowercase().as_str()) && !ignored.contains(&k)
        })
        .collect();
    query.sort_unstable();
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    format!("{scheme}://{host}{port}{}{query}", uri.path())
}

/// JSON bodies are canonicalized without idempotency tokens; form bodies are
/// sorted; anything else is compared byte for byte.
#[must_use]
pub fn normalize_body(body: &[u8]) -> Vec<u8> {
    normalize_body_ignoring(body, &[])
}

/// [`normalize_body`], also dropping the `ignored` JSON paths.
#[must_use]
pub fn normalize_body_ignoring(body: &[u8], ignored: &[&str]) -> Vec<u8> {
    if body.is_empty() {
        return Vec::new();
    }
    if let Ok(mut v) = serde_json::from_slice::<Value>(body) {
        strip_volatile(&mut v);
        for p in ignored {
            rules::strip(&mut v, p);
        }
        return canonical_json(&v).into_bytes();
    }
    if let Ok(s) = std::str::from_utf8(body)
        && is_form(s)
    {
        let mut pairs: Vec<&str> = s.split('&').collect();
        pairs.sort_unstable();
        return pairs.join("&").into_bytes();
    }
    body.to_vec()
}

fn is_form(s: &str) -> bool {
    !s.contains(char::is_whitespace)
        && s.split('&').all(|kv| {
            kv.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-%".contains(c))
            })
        })
}

fn strip_volatile(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for k in VOLATILE_JSON_KEYS {
                map.remove(*k);
            }
            map.values_mut().for_each(strip_volatile);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_volatile),
        _ => {}
    }
}

/// JSON with object keys sorted, whatever map order `serde_json` was built with.
#[must_use]
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

/// Up to 10 human-readable differences between two bodies.
#[must_use]
pub fn body_differences(recorded: &[u8], actual: &[u8]) -> Vec<String> {
    body_differences_ignoring(recorded, actual, &[])
}

/// [`body_differences`], with the `ignored` JSON paths removed from both sides.
#[must_use]
pub fn body_differences_ignoring(recorded: &[u8], actual: &[u8], ignored: &[&str]) -> Vec<String> {
    match (
        serde_json::from_slice::<Value>(recorded),
        serde_json::from_slice::<Value>(actual),
    ) {
        (Ok(mut a), Ok(mut b)) => {
            strip_volatile(&mut a);
            strip_volatile(&mut b);
            for p in ignored {
                rules::strip(&mut a, p);
                rules::strip(&mut b, p);
            }
            let mut out = Vec::new();
            json_diff("$", &a, &b, &mut out);
            out.truncate(10);
            out
        }
        _ if normalize_body(recorded) == normalize_body(actual) => Vec::new(),
        _ => vec![format!(
            "body differs ({} recorded bytes, {} actual bytes)",
            recorded.len(),
            actual.len()
        )],
    }
}

#[allow(clippy::many_single_char_names)]
fn json_diff(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = format!("{path}.{k}");
                match (ma.get(k), mb.get(k)) {
                    (Some(x), Some(y)) => json_diff(&p, x, y, out),
                    (Some(x), None) => out.push(format!("{p}: removed (was {})", short(x))),
                    (None, Some(y)) => out.push(format!("{p}: added {}", short(y))),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(xa), Value::Array(xb)) if xa.len() == xb.len() => {
            for (i, (x, y)) in xa.iter().zip(xb).enumerate() {
                json_diff(&format!("{path}[{i}]"), x, y, out);
            }
        }
        _ if a != b => out.push(format!("{path}: {} -> {}", short(a), short(b))),
        _ => {}
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::format::{Body, Headers};

    fn req(method: &str, url: &str, body: &str) -> HttpRequest {
        HttpRequest {
            method: method.into(),
            url: url.into(),
            headers: Headers::new(),
            body: Body::from_bytes(body.as_bytes()),
        }
    }

    fn resp(status: u16) -> HttpResponse {
        HttpResponse {
            status,
            headers: Headers::new(),
            body: Body::default(),
        }
    }

    fn exchange(seq: u64, request_id: Option<&str>, r: HttpRequest, status: u16) -> Record {
        Record::Exchange {
            seq,
            request_id: request_id.map(ToString::to_string),
            request: r,
            response: resp(status),
            duration_ms: 1,
        }
    }

    #[test]
    fn url_normalization() {
        assert_eq!(
            normalize_url(
                "HTTPS://S3.Amazonaws.com:443/b/k?x-id=PutObject&b=2&a=1&X-Amz-Date=20260101"
            ),
            "https://s3.amazonaws.com/b/k?a=1&b=2"
        );
        assert_eq!(
            normalize_url("http://localhost:8080/"),
            "http://localhost:8080/"
        );
    }

    #[test]
    fn json_key_order_and_tokens_ignored() {
        let a = req(
            "POST",
            "https://dynamodb.us-east-1.amazonaws.com/",
            r#"{"b":1,"a":{"y":2,"x":1},"ClientRequestToken":"t1"}"#,
        );
        let b = req(
            "POST",
            "https://dynamodb.us-east-1.amazonaws.com/",
            r#"{"a":{"x":1,"y":2},"b":1,"ClientRequestToken":"t2"}"#,
        );
        assert_eq!(request_key(&a), request_key(&b));
    }

    #[test]
    fn fifo_per_key_then_shared_pool() {
        let get = req("GET", "https://api.example.com/x", "");
        let records = vec![
            exchange(1, None, req("GET", "https://secrets/", ""), 200),
            exchange(2, Some("r1"), get.clone(), 200),
            exchange(3, Some("r1"), get.clone(), 404),
        ];
        let mut m = Matcher::from_records(&records);
        assert_eq!(
            m.take(Some("r1"), &get).map(|e| e.response.status),
            Some(200)
        );
        assert_eq!(
            m.take(Some("r1"), &get).map(|e| e.response.status),
            Some(404)
        );
        assert!(m.take(Some("r1"), &get).is_none());
        // An init-phase call can be served during an invocation (lazy clients).
        assert!(
            m.take(Some("r1"), &req("GET", "https://secrets/", ""))
                .is_some()
        );
        assert!(m.remaining(Some("r1")).is_empty());
    }

    #[test]
    fn near_miss_reports_body_difference() {
        let recorded = req("POST", "https://dynamodb/", r#"{"Item":{"id":{"S":"1"}}}"#);
        let actual = req(
            "POST",
            "https://dynamodb/",
            r#"{"Item":{"id":{"S":"1"},"extra":{"S":"x"}}}"#,
        );
        let m = Matcher::from_records(&[exchange(5, Some("r1"), recorded, 200)]);
        let miss = m.near_miss(Some("r1"), &actual).expect("near miss");
        assert_eq!(miss.seq, 5);
        assert_eq!(miss.differences, vec![r#"$.Item.extra: added {"S":"x"}"#]);
    }

    #[test]
    fn near_miss_falls_back_to_used_exchange() {
        let recorded = req("POST", "https://dynamodb/", r#"{"Item":{"id":{"S":"1"}}}"#);
        let changed = req(
            "POST",
            "https://dynamodb/",
            r#"{"Item":{"id":{"S":"1"},"status":{"S":"new"}}}"#,
        );
        let mut m = Matcher::from_records(&[exchange(2, Some("r1"), recorded.clone(), 200)]);
        assert!(m.take(Some("r1"), &recorded).is_some());
        let miss = m.near_miss(Some("r1"), &changed).expect("near miss");
        assert_eq!(miss.seq, 2);
        assert_eq!(
            miss.differences,
            vec![r#"$.Item.status: added {"S":"new"}"#]
        );
    }

    #[test]
    fn rules_make_generated_fields_match() {
        let rules: Rules = serde_json::from_value(serde_json::json!({
            "ignore": [{"paths": ["$.Item.created_at"]}]
        }))
        .expect("rules");
        let recorded = req(
            "POST",
            "https://dynamodb/",
            r#"{"Item":{"id":"1","created_at":"2026-01-01"}}"#,
        );
        let actual = req(
            "POST",
            "https://dynamodb/",
            r#"{"Item":{"id":"1","created_at":"2026-11-09"}}"#,
        );
        let mut m = Matcher::new(&[exchange(1, Some("r1"), recorded, 200)], rules);
        assert!(m.take(Some("r1"), &actual).is_some());
    }

    #[test]
    fn form_bodies_sorted() {
        assert_eq!(
            normalize_body(b"Action=SendMessage&QueueUrl=q&MessageBody=hi"),
            b"Action=SendMessage&MessageBody=hi&QueueUrl=q".to_vec()
        );
    }
}
