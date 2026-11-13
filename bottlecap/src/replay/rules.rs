//! Per-suite rules for values that legitimately change between runs:
//! timestamps, generated IDs, nonces.
//!
//! ```json
//! {
//!   "ignore": [
//!     {"operation": "PutItem", "paths": ["$.Item.created_at", "$.Item.id"]},
//!     {"host": "api.example.com", "paths": ["$..request_id", "?nonce"]},
//!     {"paths": ["$.timestamp"]}
//!   ],
//!   "ignore_response": ["$.generated_at"]
//! }
//! ```
//!
//! A path is `$` followed by `.key`, `[n]`, `[*]` or `..key` (any depth);
//! `?name` drops a query parameter. A rule applies when every selector it
//! sets (`host`, `operation`, `path_prefix`) matches the request.

use crate::replay::format::HttpRequest;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Rules {
    #[serde(default)]
    pub ignore: Vec<IgnoreRule>,
    /// Paths ignored when comparing the function's response with the recording.
    #[serde(default)]
    pub ignore_response: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct IgnoreRule {
    #[serde(default)]
    pub host: Option<String>,
    /// AWS operation from `X-Amz-Target`, e.g. `PutItem`.
    #[serde(default)]
    pub operation: Option<String>,
    /// URL path prefix, e.g. `/v1/orders`.
    #[serde(default)]
    pub path_prefix: Option<String>,
    pub paths: Vec<String>,
}

impl Rules {
    /// Reads rules from `path`; a missing file means no rules.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Body paths and query parameters to ignore for `req`.
    #[must_use]
    pub fn for_request(&self, req: &HttpRequest) -> (Vec<&str>, Vec<&str>) {
        let uri = req.url.parse::<http::Uri>().ok();
        // `http::Uri` keeps IPv6 hosts bracketed; rules may write them either way.
        let unbracket = |h: &str| h.trim_start_matches('[').trim_end_matches(']').to_string();
        let host = unbracket(uri.as_ref().and_then(|u| u.host()).unwrap_or_default());
        let url_path = uri.as_ref().map_or("/", http::Uri::path);
        let op = crate::replay::matcher::operation(req);
        let mut body = Vec::new();
        let mut query = Vec::new();
        for rule in &self.ignore {
            let applies = rule
                .host
                .as_deref()
                .is_none_or(|h| unbracket(h).eq_ignore_ascii_case(&host))
                && rule
                    .operation
                    .as_deref()
                    .is_none_or(|o| Some(o) == op.as_deref())
                && rule
                    .path_prefix
                    .as_deref()
                    .is_none_or(|p| url_path.starts_with(p));
            if !applies {
                continue;
            }
            for p in &rule.paths {
                match p.strip_prefix('?') {
                    Some(q) => query.push(q),
                    None => body.push(p.as_str()),
                }
            }
        }
        (body, query)
    }
}

#[derive(Debug, PartialEq)]
enum Segment {
    Key(String),
    Index(usize),
    Any,
    /// `..key`: the key at any depth.
    Deep(String),
}

fn parse(path: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let rest = path.strip_prefix('$').unwrap_or(path);
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;
    let read_key = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && chars[*i] != '.' && chars[*i] != '[' {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    while i < chars.len() {
        match chars[i] {
            '.' if chars.get(i + 1) == Some(&'.') => {
                i += 2;
                out.push(Segment::Deep(read_key(&mut i)));
            }
            '.' => {
                i += 1;
                out.push(Segment::Key(read_key(&mut i)));
            }
            '[' => {
                let end = chars[i..]
                    .iter()
                    .position(|c| *c == ']')
                    .map_or(chars.len(), |p| i + p);
                let inner: String = chars[i + 1..end].iter().collect();
                out.push(match inner.as_str() {
                    "*" => Segment::Any,
                    n => n.parse().map_or(
                        Segment::Key(n.trim_matches('"').to_string()),
                        Segment::Index,
                    ),
                });
                i = end + 1;
            }
            _ => out.push(Segment::Key(read_key(&mut i))),
        }
    }
    out
}

/// Removes every value `path` selects from `v`.
pub fn strip(v: &mut Value, path: &str) {
    strip_segments(v, &parse(path));
}

fn strip_segments(v: &mut Value, segs: &[Segment]) {
    let Some((first, rest)) = segs.split_first() else {
        return;
    };
    let last = rest.is_empty();
    match first {
        Segment::Key(k) => {
            if let Value::Object(map) = v {
                if last {
                    map.remove(k);
                } else if let Some(child) = map.get_mut(k) {
                    strip_segments(child, rest);
                }
            }
        }
        Segment::Index(n) => {
            if let Value::Array(items) = v {
                if last {
                    if *n < items.len() {
                        items.remove(*n);
                    }
                } else if let Some(child) = items.get_mut(*n) {
                    strip_segments(child, rest);
                }
            }
        }
        Segment::Any => match v {
            Value::Array(items) if last => items.clear(),
            Value::Object(map) if last => map.clear(),
            Value::Array(items) => items.iter_mut().for_each(|c| strip_segments(c, rest)),
            Value::Object(map) => map.values_mut().for_each(|c| strip_segments(c, rest)),
            _ => {}
        },
        Segment::Deep(k) => match v {
            Value::Object(map) => {
                if last {
                    map.remove(k);
                } else if let Some(child) = map.get_mut(k) {
                    strip_segments(child, rest);
                }
                map.values_mut().for_each(|c| strip_segments(c, segs));
            }
            Value::Array(items) => items.iter_mut().for_each(|c| strip_segments(c, segs)),
            _ => {}
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::format::{Body, Headers};
    use serde_json::json;

    #[test]
    fn strips_paths() {
        let mut v = json!({"a": {"b": 1, "c": 2}, "items": [{"id": 1, "x": 1}, {"id": 2, "x": 2}],
                           "deep": {"n": {"request_id": "r"}, "request_id": "s"}});
        strip(&mut v, "$.a.b");
        strip(&mut v, "$.items[*].id");
        strip(&mut v, "$..request_id");
        assert_eq!(
            v,
            json!({"a": {"c": 2}, "items": [{"x": 1}, {"x": 2}], "deep": {"n": {}}})
        );
    }

    #[test]
    fn rules_select_by_operation_and_host() {
        let rules: Rules = serde_json::from_value(json!({
            "ignore": [
                {"operation": "PutItem", "paths": ["$.Item.created_at"]},
                {"host": "api.example.com", "paths": ["?nonce"]},
                {"paths": ["$.ts"]}
            ]
        }))
        .expect("rules");
        let mut headers = Headers::new();
        headers.insert(
            "x-amz-target".into(),
            vec!["DynamoDB_20120810.PutItem".into()],
        );
        let put = HttpRequest {
            method: "POST".into(),
            url: "https://dynamodb.us-east-1.amazonaws.com/".into(),
            headers,
            body: Body::default(),
        };
        assert_eq!(
            rules.for_request(&put),
            (vec!["$.Item.created_at", "$.ts"], vec![])
        );
        let api = HttpRequest {
            method: "GET".into(),
            url: "https://api.example.com/x?nonce=1".into(),
            headers: Headers::new(),
            body: Body::default(),
        };
        assert_eq!(rules.for_request(&api), (vec!["$.ts"], vec!["nonce"]));
        let ipv6: Rules =
            serde_json::from_value(json!({"ignore": [{"host": "::1", "paths": ["?n"]}]}))
                .expect("rules");
        let local = HttpRequest {
            url: "http://[::1]:8088/x?n=1".into(),
            ..api
        };
        assert_eq!(ipv6.for_request(&local), (vec![], vec!["n"]));
    }
}
