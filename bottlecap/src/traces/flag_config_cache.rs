//! Experimental cache for Bottlecap's authenticated Lambda flag-config route.
//! This is forwarding state, not an SDK evaluator or a general cache library.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use tokio::{
    sync::Mutex,
    time::{Instant, timeout_at},
};

use crate::config::Config;

const MAX_SCOPES: usize = 8;
const MAX_BYTES: usize = 8 << 20;

// Resolve credentials before every read. A changed key cannot reuse another key's rules.
#[derive(Eq, Hash, PartialEq)]
struct Key(String, HeaderValue, Option<HeaderValue>);

struct Snapshot {
    headers: HeaderMap,
    body: Bytes,
    validated: Instant,
}

#[derive(Default)]
struct Entry {
    snapshot: Option<Snapshot>,
    retry_after: Option<Instant>,
}

pub(super) struct FlagConfigCache {
    ttl: Duration,
    max_age: Duration,
    pub(super) timeout: Duration,
    backoff: Duration,
    entries: Mutex<HashMap<Key, Arc<Mutex<Entry>>>>,
}

impl FlagConfigCache {
    pub(super) fn from_config(config: &Config) -> Option<Self> {
        let c = &config.ext;
        if !c.serverless_flag_config_cache_enabled {
            return None;
        }
        if c.serverless_flag_config_cache_max_age.is_zero()
            || c.serverless_flag_config_cache_timeout.is_zero()
            || c.serverless_flag_config_cache_ttl > c.serverless_flag_config_cache_max_age
            || Instant::now()
                .checked_add(c.serverless_flag_config_cache_timeout)
                .is_none()
            || Instant::now()
                .checked_add(c.serverless_flag_config_cache_failure_backoff)
                .is_none()
        {
            tracing::warn!(
                "Invalid flag configuration cache durations; keeping passthrough enabled"
            );
            return None;
        }
        Some(Self {
            ttl: c.serverless_flag_config_cache_ttl,
            max_age: c.serverless_flag_config_cache_max_age,
            timeout: c.serverless_flag_config_cache_timeout,
            backoff: c.serverless_flag_config_cache_failure_backoff,
            entries: Mutex::new(HashMap::new()),
        })
    }

    pub(super) async fn respond(
        &self,
        client: &reqwest::Client,
        mut request: reqwest::Request,
        downstream: &HeaderMap,
        deadline: Instant,
    ) -> Response {
        let Some(api_key) = request.headers().get("DD-API-KEY").cloned() else {
            return StatusCode::FORBIDDEN.into_response();
        };
        let key = Key(
            request.url().to_string(),
            api_key,
            request.headers().get(header::ACCEPT).cloned(),
        );
        let bucket = {
            let mut entries = self.entries.lock().await;
            if !entries.contains_key(&key) && entries.len() >= MAX_SCOPES {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            Arc::clone(entries.entry(key).or_default())
        };
        let Ok(mut entry) = timeout_at(deadline, bucket.lock()).await else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        if let Some(snapshot) = &entry.snapshot
            && snapshot.validated.elapsed() < self.ttl
        {
            return reply(snapshot, downstream, "memory");
        }
        if entry.retry_after.is_none_or(|at| Instant::now() >= at) {
            // The cache owns CDN revalidation, independently of the caller's SDK state.
            request.headers_mut().remove(header::IF_NONE_MATCH);
            request.headers_mut().insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("identity"),
            );
            if let Some(snapshot) = &entry.snapshot
                && let Some(etag) = snapshot.headers.get(header::ETAG)
            {
                request
                    .headers_mut()
                    .insert(header::IF_NONE_MATCH, etag.clone());
            }
            let result = timeout_at(deadline, fetch(client, request)).await;
            match result {
                Ok(Ok(Fetched::Changed(snapshot))) => entry.snapshot = Some(snapshot),
                Ok(Ok(Fetched::Unchanged)) if entry.snapshot.is_some() => {
                    if let Some(snapshot) = &mut entry.snapshot {
                        snapshot.validated = Instant::now();
                    }
                }
                // Revoked credentials or a deleted resource must not serve stale rules.
                Ok(Ok(Fetched::Rejected(status))) => {
                    entry.snapshot = None;
                    return status.into_response();
                }
                _ => {
                    entry.retry_after = Some(Instant::now() + self.backoff);
                    return self.fallback(&entry, downstream);
                }
            }
            entry.retry_after = None;
            if let Some(snapshot) = &entry.snapshot {
                return reply(snapshot, downstream, "refresh");
            }
        }
        self.fallback(&entry, downstream)
    }

    fn fallback(&self, entry: &Entry, downstream: &HeaderMap) -> Response {
        if let Some(snapshot) = &entry.snapshot
            && snapshot.validated.elapsed() <= self.max_age
        {
            return reply(snapshot, downstream, "stale");
        }
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

enum Fetched {
    Changed(Snapshot),
    Unchanged,
    Rejected(StatusCode),
}

async fn fetch(client: &reqwest::Client, request: reqwest::Request) -> Result<Fetched, ()> {
    let mut response = client.execute(request).await.map_err(|_| ())?;
    let status = response.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::Unchanged);
    }
    if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
        return Ok(Fetched::Rejected(status));
    }
    if status != StatusCode::OK
        || response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
    {
        return Err(());
    }
    let mut headers = HeaderMap::new();
    for name in [header::CONTENT_TYPE, header::ETAG, header::CACHE_CONTROL] {
        if let Some(value) = response.headers().get(&name) {
            headers.insert(name, value.clone());
        }
    }
    if !headers.contains_key(header::ETAG) {
        return Err(());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len() + chunk.len() > MAX_BYTES {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| ())?;
    let attributes = &value["data"]["attributes"];
    if attributes["format"] != "SERVER" || !attributes["flags"].is_object() {
        return Err(());
    }
    Ok(Fetched::Changed(Snapshot {
        headers,
        body: body.into(),
        validated: Instant::now(),
    }))
}

fn reply(snapshot: &Snapshot, downstream: &HeaderMap, source: &'static str) -> Response {
    let mut headers = snapshot.headers.clone();
    if let Some(etag) = headers.get(header::ETAG).cloned() {
        headers.insert("X-Config-Version", etag);
    }
    headers.insert("X-Config-Source", HeaderValue::from_static(source));
    let not_modified = downstream.get(header::IF_NONE_MATCH).is_some_and(|v| {
        snapshot
            .headers
            .get(header::ETAG)
            .is_some_and(|etag| v == etag || v == "*")
    });
    if not_modified {
        (StatusCode::NOT_MODIFIED, headers).into_response()
    } else {
        (StatusCode::OK, headers, snapshot.body.clone()).into_response()
    }
}
