//! Execution-environment cache for the feature flag configuration route.
//!
//! The Extension owns the refresh from Datadog. The tracer reads from this
//! cache over localhost, so local reads do not reach the CDN while the cached
//! configuration is fresh. Freshness uses the wall clock at read time, not a
//! background timer: Lambda freezes the environment between invocations, so
//! the first read after a thaw sees the full elapsed time and refreshes.
//!
//! The cache lives and dies with the execution environment. Each new
//! environment fetches its own copy unless a bootstrap file is fresh.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use axum::http::{HeaderValue, StatusCode};
use bytes::Bytes;
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::{debug, error, warn};

use crate::config::Config;

/// Wall clock, injectable so tests can simulate a freeze.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

#[derive(Clone, Copy, Debug)]
pub struct CacheSettings {
    /// Serve without contacting Datadog while the configuration is this young.
    pub ttl: Duration,
    /// Serve an older configuration only while it is this young, when a
    /// refresh fails or does not finish within `refresh_wait`.
    pub max_stale: Duration,
    /// Longest a read waits for the first configuration of its scope.
    pub startup_wait: Duration,
    /// Longest a read waits for a refresh when a stale configuration exists.
    pub refresh_wait: Duration,
    /// After a failed refresh, wait this long before contacting Datadog again.
    pub failure_backoff: Duration,
}

impl CacheSettings {
    #[must_use]
    pub fn from_config(config: &Config) -> Option<Self> {
        let ext = &config.ext;
        ext.feature_flags_extension_cache_enabled.then(|| Self {
            ttl: ext.feature_flags_extension_cache_ttl_seconds,
            max_stale: ext.feature_flags_extension_cache_max_stale_seconds,
            startup_wait: Duration::from_millis(ext.feature_flags_extension_cache_startup_wait_ms),
            refresh_wait: Duration::from_millis(ext.feature_flags_extension_cache_refresh_wait_ms),
            failure_backoff: FAILURE_BACKOFF,
        })
    }
}

const FAILURE_BACKOFF: Duration = Duration::from_secs(5);

/// What a refresh got from Datadog.
pub enum Fetched {
    Config {
        body: Bytes,
        etag: Option<String>,
    },
    NotModified,
    /// Datadog's status, or 502 when Datadog was unreachable.
    Failed(StatusCode),
}

/// Fetches from Datadog, sending the cached `ETag` (if any) as `If-None-Match`.
pub type Fetch = Box<dyn FnOnce(Option<String>) -> BoxFuture<'static, Fetched> + Send>;

#[derive(Clone)]
pub struct Entry {
    pub body: Bytes,
    pub etag: String,
    accepted_at: SystemTime,
    bootstrap: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Freshness {
    Fresh,
    Stale,
    Bootstrap,
}

impl Freshness {
    #[must_use]
    pub fn as_header(self) -> HeaderValue {
        HeaderValue::from_static(match self {
            Freshness::Fresh => "fresh",
            Freshness::Stale => "stale",
            Freshness::Bootstrap => "bootstrap",
        })
    }
}

pub enum Lookup {
    Serve {
        entry: Entry,
        freshness: Freshness,
        age: Duration,
    },
    Unavailable(StatusCode),
}

#[derive(Default)]
struct ScopeState {
    entry: Option<Entry>,
    /// Set while a refresh runs; flips to `true` when it ends.
    refreshing: Option<watch::Receiver<bool>>,
    last_failure: Option<(SystemTime, StatusCode)>,
}

#[derive(Clone)]
pub struct FlagConfigCache {
    settings: CacheSettings,
    clock: Clock,
    scopes: Arc<Mutex<HashMap<String, Arc<Mutex<ScopeState>>>>>,
    bootstrap: Option<(String, Entry)>,
    upstream_requests: Arc<AtomicU64>,
}

impl FlagConfigCache {
    #[must_use]
    pub fn new(settings: CacheSettings, clock: Clock) -> Self {
        Self {
            settings,
            clock,
            scopes: Arc::default(),
            bootstrap: None,
            upstream_requests: Arc::default(),
        }
    }

    /// Seeds the scope for `query` (what the Extension sends Datadog when the
    /// tracer adds no query) with a configuration shipped with the function.
    /// Its age counts from its `createdAt`, so an old file is only a fallback.
    #[must_use]
    pub fn with_bootstrap(mut self, query: String, body: Bytes) -> Self {
        let Some(created_at) = valid_config(&body).and_then(|v| {
            let created = v.get("createdAt")?.as_str()?.to_string();
            chrono::DateTime::parse_from_rfc3339(&created).ok()
        }) else {
            error!(
                "TRACE_AGENT | Ignoring the flag config bootstrap file: not a valid configuration"
            );
            return self;
        };
        let entry = Entry {
            etag: etag_for(&body),
            body,
            accepted_at: created_at.into(),
            bootstrap: true,
        };
        self.bootstrap = Some((query, entry));
        self
    }

    /// Requests made to Datadog since the cache was created.
    #[must_use]
    pub fn upstream_requests(&self) -> u64 {
        self.upstream_requests.load(Ordering::Relaxed)
    }

    /// `scope` must identify everything that changes Datadog's answer (API
    /// key and query), so scopes never see each other's configuration.
    pub async fn get(&self, scope: &str, query: &str, fetch: Fetch) -> Lookup {
        let state = self.scope(scope, query);
        let (mut done, wait) = {
            let mut s = state.lock().expect("lock poisoned");
            let now = (self.clock)();
            if let Some(lookup) = self.usable(&s, now, true) {
                return lookup;
            }
            let backing_off = s.last_failure.is_some_and(|(at, _)| {
                now.duration_since(at).unwrap_or_default() < self.settings.failure_backoff
            });
            if backing_off && s.refreshing.is_none() {
                return self.usable(&s, now, false).unwrap_or_else(|| {
                    Lookup::Unavailable(s.last_failure.map_or(StatusCode::BAD_GATEWAY, |f| f.1))
                });
            }
            let done = if let Some(done) = &s.refreshing {
                done.clone()
            } else {
                let done = self.spawn_refresh(&state, &s, fetch);
                s.refreshing = Some(done.clone());
                done
            };
            let wait = if s.entry.is_some() {
                self.settings.refresh_wait
            } else {
                self.settings.startup_wait
            };
            (done, wait)
        };

        // A timeout here leaves the refresh running for later readers.
        let _ = tokio::time::timeout(wait, done.wait_for(|d| *d)).await;

        let s = state.lock().expect("lock poisoned");
        self.usable(&s, (self.clock)(), false).unwrap_or_else(|| {
            Lookup::Unavailable(match (s.refreshing.is_some(), s.last_failure) {
                (true, _) => StatusCode::GATEWAY_TIMEOUT,
                (false, Some((_, status))) => status,
                (false, None) => StatusCode::BAD_GATEWAY,
            })
        })
    }

    fn scope(&self, scope: &str, query: &str) -> Arc<Mutex<ScopeState>> {
        let mut scopes = self.scopes.lock().expect("lock poisoned");
        Arc::clone(scopes.entry(scope.to_string()).or_insert_with(|| {
            let entry = self
                .bootstrap
                .as_ref()
                .filter(|(q, _)| q == query)
                .map(|(_, e)| e.clone());
            Arc::new(Mutex::new(ScopeState {
                entry,
                ..ScopeState::default()
            }))
        }))
    }

    /// With `fresh_only`, returns only a configuration within the TTL.
    fn usable(&self, s: &ScopeState, now: SystemTime, fresh_only: bool) -> Option<Lookup> {
        let entry = s.entry.as_ref()?;
        let age = now.duration_since(entry.accepted_at).unwrap_or_default();
        let freshness = if age <= self.settings.ttl {
            Freshness::Fresh
        } else if fresh_only {
            return None;
        } else if entry.bootstrap {
            Freshness::Bootstrap
        } else if age <= self.settings.max_stale {
            Freshness::Stale
        } else {
            return None;
        };
        Some(Lookup::Serve {
            entry: entry.clone(),
            freshness,
            age,
        })
    }

    fn spawn_refresh(
        &self,
        state: &Arc<Mutex<ScopeState>>,
        s: &ScopeState,
        fetch: Fetch,
    ) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        // Revalidate only a configuration Datadog sent, not the bootstrap file.
        let etag = s
            .entry
            .as_ref()
            .filter(|e| !e.bootstrap)
            .map(|e| e.etag.clone());
        let state = Arc::clone(state);
        let clock = Arc::clone(&self.clock);
        self.upstream_requests.fetch_add(1, Ordering::Relaxed);
        // Its own task: a tracer that disconnects must not cancel the refresh
        // that other readers wait on.
        tokio::spawn(async move {
            let fetched = fetch(etag).await;
            let mut s = state.lock().expect("lock poisoned");
            let now = clock();
            match fetched {
                Fetched::Config { body, etag } if valid_config(&body).is_some() => {
                    debug!("TRACE_AGENT | Accepted a new flag config");
                    s.entry = Some(Entry {
                        etag: etag.unwrap_or_else(|| etag_for(&body)),
                        body,
                        accepted_at: now,
                        bootstrap: false,
                    });
                    s.last_failure = None;
                }
                Fetched::Config { .. } => {
                    warn!(
                        "TRACE_AGENT | Rejected an invalid flag config, keeping the previous one"
                    );
                    s.last_failure = Some((now, StatusCode::BAD_GATEWAY));
                }
                Fetched::NotModified => {
                    if let Some(entry) = s.entry.as_mut().filter(|e| !e.bootstrap) {
                        entry.accepted_at = now;
                        s.last_failure = None;
                    } else {
                        s.last_failure = Some((now, StatusCode::BAD_GATEWAY));
                    }
                }
                Fetched::Failed(status) => {
                    warn!("TRACE_AGENT | Flag config refresh failed: {status}");
                    s.last_failure = Some((now, status));
                }
            }
            s.refreshing = None;
            let _ = tx.send(true);
        });
        rx
    }
}

/// Same scope for the same API key and query; the key itself is not kept.
#[must_use]
pub fn scope_key(api_key: &str, query: &str) -> String {
    let digest = Sha256::digest(api_key.as_bytes());
    format!("{}|{query}", hex(&digest[..8]))
}

/// The CDN's JSON:API envelope around a Universal Flag Configuration; returns
/// the configuration.
fn valid_config(body: &[u8]) -> Option<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let data = value.get_mut("data")?;
    if data.get("type")?.as_str()? != "universal-flag-configuration" {
        return None;
    }
    let config = data.get_mut("attributes")?.take();
    config.get("flags")?.is_object().then_some(config)
}

fn etag_for(body: &[u8]) -> String {
    format!("\"{}\"", hex(&Sha256::digest(body)[..16]))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use tokio::time::Instant;

    const V1: &str = r#"{"data":{"type":"universal-flag-configuration","attributes":{"createdAt":"2026-11-17T00:00:00Z","format":"SERVER","environment":{"name":"prod"},"flags":{"a":{}}}}}"#;
    const V2: &str = r#"{"data":{"type":"universal-flag-configuration","attributes":{"createdAt":"2026-11-17T00:01:00Z","format":"SERVER","environment":{"name":"prod"},"flags":{"b":{}}}}}"#;

    fn settings() -> CacheSettings {
        CacheSettings {
            ttl: Duration::from_secs(30),
            max_stale: Duration::from_secs(300),
            startup_wait: Duration::from_secs(3),
            refresh_wait: Duration::from_millis(500),
            failure_backoff: Duration::from_secs(5),
        }
    }

    /// A wall clock that only moves when the test says so, like a frozen
    /// environment whose clock jumps when it thaws.
    #[derive(Clone)]
    struct TestClock(Arc<Mutex<SystemTime>>);

    impl TestClock {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_960_000),
            )))
        }
        fn advance(&self, by: Duration) {
            *self.0.lock().expect("lock") += by;
        }
        fn clock(&self) -> Clock {
            let now = Arc::clone(&self.0);
            Arc::new(move || *now.lock().expect("lock"))
        }
    }

    /// Answers fetches in order and counts them.
    #[derive(Clone)]
    struct Upstream {
        replies: Arc<Mutex<VecDeque<(Duration, Fetched)>>>,
        calls: Arc<AtomicUsize>,
        etags: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl Upstream {
        fn new(replies: Vec<(Duration, Fetched)>) -> Self {
            Self {
                replies: Arc::new(Mutex::new(replies.into())),
                calls: Arc::default(),
                etags: Arc::default(),
            }
        }
        fn fetch(&self) -> Fetch {
            let this = self.clone();
            Box::new(move |etag| {
                this.calls.fetch_add(1, Ordering::SeqCst);
                this.etags.lock().expect("lock").push(etag);
                let (delay, reply) = this
                    .replies
                    .lock()
                    .expect("lock")
                    .pop_front()
                    .expect("unexpected fetch");
                Box::pin(async move {
                    tokio::time::sleep(delay).await;
                    reply
                })
            })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    fn config(body: &str) -> (Duration, Fetched) {
        (
            Duration::from_millis(10),
            Fetched::Config {
                body: Bytes::from(body.to_string()),
                etag: None,
            },
        )
    }

    fn served(lookup: Lookup) -> (String, Freshness) {
        match lookup {
            Lookup::Serve {
                entry, freshness, ..
            } => (
                String::from_utf8(entry.body.to_vec()).expect("utf8"),
                freshness,
            ),
            Lookup::Unavailable(status) => panic!("unavailable: {status}"),
        }
    }

    fn unavailable(lookup: Lookup) -> StatusCode {
        match lookup {
            Lookup::Unavailable(status) => status,
            Lookup::Serve { freshness, .. } => panic!("served {freshness:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_reads_reuse_one_fresh_fetch() {
        let clock = TestClock::new();
        let cache = FlagConfigCache::new(settings(), clock.clock());
        let upstream = Upstream::new(vec![config(V1)]);

        for _ in 0..100 {
            assert_eq!(served(cache.get("s", "", upstream.fetch()).await).0, V1);
            clock.advance(Duration::from_millis(100));
        }

        assert_eq!(upstream.calls(), 1);
        assert_eq!(cache.upstream_requests(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_reads_share_one_refresh() {
        let cache = FlagConfigCache::new(settings(), TestClock::new().clock());
        let upstream = Upstream::new(vec![(
            Duration::from_millis(200),
            Fetched::Config {
                body: Bytes::from(V1),
                etag: None,
            },
        )]);

        let reads = (0..32).map(|_| {
            let cache = cache.clone();
            let fetch = upstream.fetch();
            tokio::spawn(async move { served(cache.get("s", "", fetch).await).0 })
        });
        for read in futures::future::join_all(reads).await {
            assert_eq!(read.expect("task"), V1);
        }

        assert_eq!(upstream.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_read_after_a_freeze_refreshes() {
        let clock = TestClock::new();
        let cache = FlagConfigCache::new(settings(), clock.clock());
        let upstream = Upstream::new(vec![config(V1), config(V2)]);
        served(cache.get("s", "", upstream.fetch()).await);

        // Frozen for less than the TTL: no timer ran, and none is needed.
        clock.advance(Duration::from_secs(29));
        assert_eq!(served(cache.get("s", "", upstream.fetch()).await).0, V1);
        assert_eq!(upstream.calls(), 1);

        // Frozen past the TTL: the read itself sees the elapsed time.
        clock.advance(Duration::from_secs(600));
        assert_eq!(
            served(cache.get("s", "", upstream.fetch()).await),
            (V2.to_string(), Freshness::Fresh)
        );
        assert_eq!(upstream.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_invalid_refresh_keeps_the_accepted_config() {
        let clock = TestClock::new();
        let cache = FlagConfigCache::new(settings(), clock.clock());
        let upstream = Upstream::new(vec![
            config(V1),
            config(r#"{"data":{"type":"universal-flag-configuration","attributes":{"flags":[]}}}"#),
            config("<html>truncated"),
        ]);
        served(cache.get("s", "", upstream.fetch()).await);

        clock.advance(Duration::from_secs(31));
        assert_eq!(
            served(cache.get("s", "", upstream.fetch()).await),
            (V1.to_string(), Freshness::Stale)
        );
        // Backs off instead of asking Datadog on every read.
        assert_eq!(
            served(cache.get("s", "", upstream.fetch()).await).1,
            Freshness::Stale
        );
        assert_eq!(upstream.calls(), 2);

        clock.advance(Duration::from_secs(6));
        assert_eq!(served(cache.get("s", "", upstream.fetch()).await).0, V1);
        assert_eq!(upstream.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_refresh_serves_stale_only_until_max_stale() {
        let clock = TestClock::new();
        let cache = FlagConfigCache::new(settings(), clock.clock());
        let failed = || {
            (
                Duration::ZERO,
                Fetched::Failed(StatusCode::SERVICE_UNAVAILABLE),
            )
        };
        let upstream = Upstream::new(vec![config(V1), failed(), failed()]);
        served(cache.get("s", "", upstream.fetch()).await);

        clock.advance(Duration::from_secs(299));
        assert_eq!(
            served(cache.get("s", "", upstream.fetch()).await),
            (V1.to_string(), Freshness::Stale)
        );

        clock.advance(Duration::from_secs(10));
        assert_eq!(
            unavailable(cache.get("s", "", upstream.fetch()).await),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(upstream.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn the_startup_wait_is_bounded() {
        let cache = FlagConfigCache::new(settings(), TestClock::new().clock());
        let upstream = Upstream::new(vec![(
            Duration::from_secs(3600),
            Fetched::Config {
                body: Bytes::from(V1),
                etag: None,
            },
        )]);

        let started = Instant::now();
        let status = unavailable(cache.get("s", "", upstream.fetch()).await);

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(started.elapsed(), settings().startup_wait);
    }

    #[tokio::test(start_paused = true)]
    async fn the_refresh_wait_is_bounded_and_the_refresh_finishes_later() {
        let clock = TestClock::new();
        let cache = FlagConfigCache::new(settings(), clock.clock());
        let slow = (
            Duration::from_secs(2),
            Fetched::Config {
                body: Bytes::from(V2),
                etag: None,
            },
        );
        let upstream = Upstream::new(vec![config(V1), slow]);
        served(cache.get("s", "", upstream.fetch()).await);
        clock.advance(Duration::from_secs(31));

        let started = Instant::now();
        assert_eq!(
            served(cache.get("s", "", upstream.fetch()).await),
            (V1.to_string(), Freshness::Stale)
        );
        assert_eq!(started.elapsed(), settings().refresh_wait);

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            served(cache.get("s", "", upstream.fetch()).await),
            (V2.to_string(), Freshness::Fresh)
        );
        assert_eq!(upstream.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn not_modified_renews_the_config_and_sends_its_etag() {
        let clock = TestClock::new();
        let cache = FlagConfigCache::new(settings(), clock.clock());
        let upstream = Upstream::new(vec![
            (
                Duration::ZERO,
                Fetched::Config {
                    body: Bytes::from(V1),
                    etag: Some("\"v1\"".to_string()),
                },
            ),
            (Duration::ZERO, Fetched::NotModified),
        ]);
        served(cache.get("s", "", upstream.fetch()).await);
        clock.advance(Duration::from_secs(31));

        assert_eq!(
            served(cache.get("s", "", upstream.fetch()).await),
            (V1.to_string(), Freshness::Fresh)
        );
        clock.advance(Duration::from_secs(29));
        served(cache.get("s", "", upstream.fetch()).await);

        assert_eq!(upstream.calls(), 2);
        assert_eq!(
            *upstream.etags.lock().expect("lock"),
            vec![None, Some("\"v1\"".to_string())]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn scopes_never_see_each_other() {
        let cache = FlagConfigCache::new(settings(), TestClock::new().clock());
        let prod = Upstream::new(vec![config(V1)]);
        let staging = Upstream::new(vec![config(V2)]);
        let prod_scope = scope_key("key", "dd_env=prod");
        let staging_scope = scope_key("key", "dd_env=staging");
        let other_key = scope_key("other-key", "dd_env=prod");

        assert_eq!(
            served(cache.get(&prod_scope, "dd_env=prod", prod.fetch()).await).0,
            V1
        );
        assert_eq!(
            served(
                cache
                    .get(&staging_scope, "dd_env=staging", staging.fetch())
                    .await
            )
            .0,
            V2
        );
        assert_ne!(prod_scope, other_key);
        assert!(!prod_scope.contains("key|"), "the API key is not kept");
        assert_eq!((prod.calls(), staging.calls()), (1, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn each_new_environment_fetches_its_own_config() {
        let upstream = Upstream::new((0..16).map(|_| config(V1)).collect());
        for _ in 0..16 {
            // A cold start: new process, new cache.
            let cache = FlagConfigCache::new(settings(), TestClock::new().clock());
            served(cache.get("s", "", upstream.fetch()).await);
        }
        assert_eq!(upstream.calls(), 16);
    }

    #[tokio::test(start_paused = true)]
    async fn a_fresh_bootstrap_file_avoids_the_initial_fetch() {
        let clock = TestClock::new();
        let created =
            chrono::DateTime::<chrono::Utc>::from((clock.clock())() - Duration::from_secs(10))
                .to_rfc3339();
        let body = V1.replace("2026-11-17T00:00:00Z", &created);
        let cache = FlagConfigCache::new(settings(), clock.clock())
            .with_bootstrap("dd_env=prod".to_string(), Bytes::from(body.clone()));
        let upstream = Upstream::new(vec![]);

        assert_eq!(
            served(cache.get("s", "dd_env=prod", upstream.fetch()).await),
            (body, Freshness::Fresh)
        );
        assert_eq!(upstream.calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn an_old_bootstrap_file_is_only_a_fallback() {
        let cache = FlagConfigCache::new(settings(), TestClock::new().clock())
            .with_bootstrap("dd_env=prod".to_string(), Bytes::from(V1));
        let upstream = Upstream::new(vec![
            (Duration::ZERO, Fetched::Failed(StatusCode::BAD_GATEWAY)),
            config(V2),
        ]);

        // Datadog is tried first; its failure falls back to the file.
        assert_eq!(
            served(cache.get("s", "dd_env=prod", upstream.fetch()).await),
            (V1.to_string(), Freshness::Bootstrap)
        );
        // Another scope never gets the file.
        assert_eq!(
            served(cache.get("t", "dd_env=staging", upstream.fetch()).await).0,
            V2
        );
        assert_eq!(upstream.calls(), 2);
    }
}
