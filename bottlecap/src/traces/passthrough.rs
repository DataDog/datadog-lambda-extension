//! Pass-through routes on the trace agent.
//!
//! For a fixed list of paths, the Extension forwards the tracer's request to
//! one Datadog endpoint right away, adds the API key it resolved, and returns
//! Datadog's reply unchanged. This lets tracers reach Datadog when only the
//! Extension can read the key (for example `DD_API_KEY_SECRET_ARN`).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::{
    Router,
    extract::{RawQuery, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use dogstatsd::api_key::ApiKeyFactory;
use tokio::time::{Instant, timeout_at};
use tracing::{debug, error};

use crate::config::Config;
use crate::traces::flag_config_cache::{
    CacheSettings, Fetch, Fetched, FlagConfigCache, Lookup, scope_key,
};

/// Feature flag configuration (rules) used by the tracers' flag providers.
pub const FLAG_CONFIG_PATH: &str = "/api/v2/feature-flagging/config/rules-based/server";
const FLAG_CONFIG_SUBDOMAIN: &str = "ufc-server.ff-cdn";
/// Tells the tracer whether a cached reply was fresh, stale or the bootstrap file.
pub const CACHE_STATUS_HEADER: &str = "x-datadog-flag-config-cache";

/// Covers resolving the API key and the request to Datadog. Below the tracers'
/// default 5 second request timeout, so the tracer sees our error instead of
/// timing out itself.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(4);

const FORWARDED_REQUEST_HEADERS: [HeaderName; 3] = [
    header::ACCEPT,
    header::ACCEPT_ENCODING,
    header::IF_NONE_MATCH,
];
const FORWARDED_RESPONSE_HEADERS: [HeaderName; 4] = [
    header::CONTENT_TYPE,
    header::CONTENT_ENCODING,
    header::ETAG,
    header::CACHE_CONTROL,
];

#[derive(Clone)]
pub struct Passthrough {
    api_key_factory: Arc<ApiKeyFactory>,
    /// Must not follow redirects, so the API key only goes to `flag_config_base_url`.
    client: reqwest::Client,
    flag_config_base_url: String,
    env: Option<String>,
    timeout: Duration,
    /// When set, flag config is served from the execution environment's cache.
    cache: Option<FlagConfigCache>,
}

impl Passthrough {
    #[must_use]
    pub fn new(
        config: &Config,
        api_key_factory: Arc<ApiKeyFactory>,
        client: reqwest::Client,
    ) -> Self {
        #[allow(unused_mut)]
        let mut flag_config_base_url = format!("https://{FLAG_CONFIG_SUBDOMAIN}.{}", config.site);
        // Lets the local test harness stand in for the CDN.
        #[cfg(feature = "test-mode")]
        if let Ok(url) = std::env::var("DD_TESTMODE_FEATURE_FLAGS_CDN_URL") {
            flag_config_base_url = url;
        }
        let mut passthrough = Self {
            api_key_factory,
            client,
            flag_config_base_url,
            // `DD_ENV` wins over an `env` tag in `DD_TAGS`, like in the tracers.
            env: config
                .env
                .clone()
                .or_else(|| config.tags.get("env").cloned()),
            timeout: UPSTREAM_TIMEOUT,
            cache: None,
        };
        if let Some(settings) = CacheSettings::from_config(config) {
            let mut cache = FlagConfigCache::new(settings, Arc::new(SystemTime::now));
            if let Some(path) = &config.ext.feature_flags_extension_cache_bootstrap_file {
                match std::fs::read(path) {
                    Ok(body) => {
                        let query = passthrough
                            .upstream_url(None)
                            .query()
                            .unwrap_or("")
                            .to_string();
                        cache = cache.with_bootstrap(query, body.into());
                    }
                    Err(e) => {
                        error!("TRACE_AGENT | Can't read the flag config bootstrap file: {e}");
                    }
                }
            }
            passthrough.cache = Some(cache);
        }
        passthrough
    }

    pub fn router(self) -> Router {
        Router::new()
            .route(FLAG_CONFIG_PATH, get(flag_config))
            .with_state(self)
    }

    /// With the cache on, starts fetching the default configuration now, so
    /// it can arrive while the runtime initializes.
    pub fn prefetch(&self) {
        if self.cache.is_none() {
            return;
        }
        let passthrough = self.clone();
        tokio::spawn(async move {
            let _ = flag_config(State(passthrough), RawQuery(None), HeaderMap::new()).await;
        });
    }

    /// The tracer drops `dd_env` when it uses a custom base URL, so add ours.
    fn upstream_url(&self, query: Option<&str>) -> reqwest::Url {
        let mut url =
            reqwest::Url::parse(&format!("{}{FLAG_CONFIG_PATH}", self.flag_config_base_url))
                .unwrap_or_else(|_| {
                    reqwest::Url::parse("https://invalid.invalid").expect("valid url")
                });
        url.set_query(query);
        let has_env = url.query_pairs().any(|(k, _)| k == "dd_env");
        if let (Some(env), false) = (&self.env, has_env) {
            url.query_pairs_mut().append_pair("dd_env", env);
        }
        url
    }
}

async fn flag_config(
    State(passthrough): State<Passthrough>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let deadline = Instant::now() + passthrough.timeout;
    // Resolve the key in its own task: if the tracer disconnects, this handler
    // is dropped, and a key load cancelled midway would leave every other
    // caller of the factory waiting on it forever.
    let factory = Arc::clone(&passthrough.api_key_factory);
    let api_key = match timeout_at(
        deadline,
        tokio::spawn(async move { factory.get_api_key().await }),
    )
    .await
    {
        Ok(Ok(Some(api_key))) => api_key,
        // 403 makes the flag provider log the failure without retrying.
        Ok(Ok(None)) => {
            error!(
                "TRACE_AGENT | Failed to resolve API key, can't forward the flag config request"
            );
            return (StatusCode::FORBIDDEN, "Datadog API key unavailable").into_response();
        }
        // A 5xx is retried with backoff, and the key load keeps running.
        Ok(Err(e)) => {
            error!("TRACE_AGENT | API key resolution failed: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Err(_) => {
            error!("TRACE_AGENT | Timed out resolving the API key for the flag config request");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };

    let url = passthrough.upstream_url(query.as_deref());
    if let Some(cache) = &passthrough.cache {
        return cached(&passthrough, cache, url, api_key, &headers).await;
    }

    let mut request = passthrough
        .client
        .get(url)
        .timeout(deadline.saturating_duration_since(Instant::now()))
        .header("DD-API-KEY", api_key);
    for name in FORWARDED_REQUEST_HEADERS {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value);
        }
    }

    let upstream = match request.send().await {
        Ok(upstream) => upstream,
        Err(e) => {
            error!("TRACE_AGENT | Flag config request to Datadog failed: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };

    let status = upstream.status();
    let mut response_headers = HeaderMap::new();
    for name in FORWARDED_RESPONSE_HEADERS {
        if let Some(value) = upstream.headers().get(&name) {
            response_headers.insert(name, value.clone());
        }
    }
    match upstream.bytes().await {
        Ok(body) => {
            debug!("TRACE_AGENT | Forwarded flag config request: {status}");
            (status, response_headers, body).into_response()
        }
        Err(e) => {
            error!("TRACE_AGENT | Failed to read the flag config response: {e}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

async fn cached(
    passthrough: &Passthrough,
    cache: &FlagConfigCache,
    url: reqwest::Url,
    api_key: String,
    headers: &HeaderMap,
) -> Response {
    let query = url.query().unwrap_or("").to_string();
    let scope = scope_key(&api_key, &query);
    let client = passthrough.client.clone();
    let fetch: Fetch = Box::new(move |etag| {
        Box::pin(async move {
            let mut request = client
                .get(url)
                .timeout(UPSTREAM_TIMEOUT)
                .header("DD-API-KEY", api_key)
                .header(header::ACCEPT, "application/json");
            if let Some(etag) = etag {
                request = request.header(header::IF_NONE_MATCH, etag);
            }
            let upstream = match request.send().await {
                Ok(upstream) => upstream,
                Err(e) => {
                    error!("TRACE_AGENT | Flag config request to Datadog failed: {e}");
                    return Fetched::Failed(StatusCode::BAD_GATEWAY);
                }
            };
            match upstream.status() {
                StatusCode::NOT_MODIFIED => Fetched::NotModified,
                status if status.is_success() => {
                    let etag = upstream
                        .headers()
                        .get(header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    match upstream.bytes().await {
                        Ok(body) => Fetched::Config { body, etag },
                        Err(_) => Fetched::Failed(StatusCode::BAD_GATEWAY),
                    }
                }
                status => Fetched::Failed(status),
            }
        })
    });

    match cache.get(&scope, &query, fetch).await {
        Lookup::Serve {
            entry,
            freshness,
            age,
        } => {
            let mut response_headers = HeaderMap::new();
            if let Ok(etag) = HeaderValue::from_str(&entry.etag) {
                response_headers.insert(header::ETAG, etag);
            }
            response_headers.insert(header::AGE, HeaderValue::from(age.as_secs()));
            response_headers.insert(CACHE_STATUS_HEADER, freshness.as_header());
            let unchanged = headers
                .get(header::IF_NONE_MATCH)
                .is_some_and(|v| v.as_bytes() == entry.etag.as_bytes());
            if unchanged {
                return (StatusCode::NOT_MODIFIED, response_headers).into_response();
            }
            response_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (StatusCode::OK, response_headers, entry.body).into_response()
        }
        Lookup::Unavailable(status) => status.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use httpmock::prelude::*;
    use tower::ServiceExt;

    fn passthrough(base_url: String, api_key_factory: ApiKeyFactory) -> Passthrough {
        let config = Arc::new(Config {
            http_protocol: Some("http1".to_string()),
            ..Config::default()
        });
        Passthrough {
            api_key_factory: Arc::new(api_key_factory),
            client: crate::http::get_client_without_redirects(&config).expect("client"),
            flag_config_base_url: base_url,
            env: Some("prod".to_string()),
            timeout: UPSTREAM_TIMEOUT,
            cache: None,
        }
    }

    async fn call(passthrough: Passthrough, uri: &str, if_none_match: Option<&str>) -> Response {
        let mut request = Request::get(uri);
        if let Some(etag) = if_none_match {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        passthrough
            .router()
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn forwards_with_api_key_and_env_and_returns_the_reply() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(FLAG_CONFIG_PATH)
                    .header("DD-API-KEY", "test-key")
                    .query_param("dd_env", "prod");
                then.status(200)
                    .header("ETag", "\"abc\"")
                    .header("Content-Encoding", "gzip")
                    .body("rules");
            })
            .await;

        let response = call(
            passthrough(server.base_url(), ApiKeyFactory::new("test-key")),
            FLAG_CONFIG_PATH,
            None,
        )
        .await;

        mock.assert_async().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::ETAG], "\"abc\"");
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"rules");
    }

    #[tokio::test]
    async fn passes_not_modified_through() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(FLAG_CONFIG_PATH)
                    .header("If-None-Match", "\"abc\"");
                then.status(304);
            })
            .await;

        let response = call(
            passthrough(server.base_url(), ApiKeyFactory::new("test-key")),
            FLAG_CONFIG_PATH,
            Some("\"abc\""),
        )
        .await;

        mock.assert_async().await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn keeps_an_existing_env_query() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(FLAG_CONFIG_PATH)
                    .query_param("dd_env", "staging");
                then.status(200);
            })
            .await;

        let response = call(
            passthrough(server.base_url(), ApiKeyFactory::new("test-key")),
            &format!("{FLAG_CONFIG_PATH}?dd_env=staging"),
            None,
        )
        .await;

        mock.assert_async().await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn replies_forbidden_without_an_api_key() {
        let no_key = ApiKeyFactory::new_from_resolver(Arc::new(|| Box::pin(async { None })), None);

        let response = call(
            passthrough("http://127.0.0.1:1".to_string(), no_key),
            FLAG_CONFIG_PATH,
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn replies_bad_gateway_when_datadog_is_unreachable() {
        let response = call(
            passthrough(
                "http://127.0.0.1:1".to_string(),
                ApiKeyFactory::new("test-key"),
            ),
            FLAG_CONFIG_PATH,
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn env_falls_back_to_the_dd_tags_env() {
        let from_tags = |env: Option<&str>| {
            let config = Config {
                env: env.map(str::to_string),
                tags: [("env".to_string(), "from-tags".to_string())].into(),
                ..Config::default()
            };
            Passthrough::new(
                &config,
                Arc::new(ApiKeyFactory::new("k")),
                reqwest::Client::new(),
            )
            .env
        };
        assert_eq!(from_tags(None).as_deref(), Some("from-tags"));
        assert_eq!(from_tags(Some("prod")).as_deref(), Some("prod"));
    }

    #[tokio::test]
    async fn does_not_follow_redirects() {
        let elsewhere = MockServer::start_async().await;
        let leaked = elsewhere
            .mock_async(|when, then| {
                when.any_request();
                then.status(200);
            })
            .await;
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path(FLAG_CONFIG_PATH);
                then.status(302)
                    .header("Location", elsewhere.url(FLAG_CONFIG_PATH));
            })
            .await;

        let response = call(
            passthrough(server.base_url(), ApiKeyFactory::new("test-key")),
            FLAG_CONFIG_PATH,
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(leaked.hits_async().await, 0);
    }

    #[tokio::test]
    async fn slow_api_key_times_out_and_keeps_loading() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(FLAG_CONFIG_PATH)
                    .header("DD-API-KEY", "slow-key");
                then.status(200);
            })
            .await;
        let slow_key = ApiKeyFactory::new_from_resolver(
            Arc::new(|| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Some("slow-key".to_string())
                })
            }),
            None,
        );
        let passthrough = Passthrough {
            timeout: Duration::from_millis(100),
            ..passthrough(server.base_url(), slow_key)
        };

        let first = call(passthrough.clone(), FLAG_CONFIG_PATH, None).await;
        assert_eq!(first.status(), StatusCode::BAD_GATEWAY);

        // The load finishes in the background; a load cancelled midway would
        // leave the factory waiting forever and the next request would hang.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = tokio::time::timeout(
            Duration::from_secs(5),
            call(passthrough, FLAG_CONFIG_PATH, None),
        )
        .await
        .expect("the key load never finished");
        assert_eq!(second.status(), StatusCode::OK);
        mock.assert_async().await;
    }

    const UFC: &str = r#"{"data":{"type":"universal-flag-configuration","attributes":{"flags":{},"environment":{"name":"prod"}}}}"#;

    fn cached(passthrough: Passthrough) -> Passthrough {
        let settings = CacheSettings {
            ttl: Duration::from_secs(30),
            max_stale: Duration::from_secs(300),
            startup_wait: Duration::from_secs(3),
            refresh_wait: Duration::from_millis(500),
            failure_backoff: Duration::from_secs(5),
        };
        Passthrough {
            cache: Some(FlagConfigCache::new(settings, Arc::new(SystemTime::now))),
            ..passthrough
        }
    }

    #[tokio::test]
    async fn cache_serves_repeated_reads_from_one_request() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(FLAG_CONFIG_PATH)
                    .header("DD-API-KEY", "test-key")
                    .query_param("dd_env", "prod");
                then.status(200).header("ETag", "\"v1\"").body(UFC);
            })
            .await;
        let passthrough = cached(passthrough(
            server.base_url(),
            ApiKeyFactory::new("test-key"),
        ));

        for _ in 0..10 {
            let response = call(passthrough.clone(), FLAG_CONFIG_PATH, None).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[CACHE_STATUS_HEADER], "fresh");
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            assert_eq!(&body[..], UFC.as_bytes());
        }
        let unchanged = call(passthrough, FLAG_CONFIG_PATH, Some("\"v1\"")).await;

        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(mock.hits_async().await, 1);
    }

    #[tokio::test]
    async fn cache_keeps_environments_apart() {
        let server = MockServer::start_async().await;
        for env in ["prod", "staging"] {
            server
                .mock_async(|when, then| {
                    when.method(GET)
                        .path(FLAG_CONFIG_PATH)
                        .query_param("dd_env", env);
                    then.status(200)
                        .body(UFC.replace("\"prod\"", &format!("\"{env}\"")));
                })
                .await;
        }
        let passthrough = cached(passthrough(
            server.base_url(),
            ApiKeyFactory::new("test-key"),
        ));

        for env in ["prod", "staging", "prod"] {
            let response = call(
                passthrough.clone(),
                &format!("{FLAG_CONFIG_PATH}?dd_env={env}"),
                None,
            )
            .await;
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            assert!(
                String::from_utf8_lossy(&body).contains(&format!(r#""name":"{env}""#)),
                "{env} got {body:?}"
            );
        }
    }

    #[tokio::test]
    async fn cache_passes_an_auth_failure_through() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path(FLAG_CONFIG_PATH);
                then.status(403);
            })
            .await;
        let passthrough = cached(passthrough(
            server.base_url(),
            ApiKeyFactory::new("bad-key"),
        ));

        let response = call(passthrough, FLAG_CONFIG_PATH, None).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn does_not_forward_other_paths() {
        let response = call(
            passthrough(
                "http://127.0.0.1:1".to_string(),
                ApiKeyFactory::new("test-key"),
            ),
            "/api/v2/other",
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
