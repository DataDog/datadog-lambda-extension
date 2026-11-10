//! Pass-through routes on the trace agent.
//!
//! For a fixed list of paths, the Extension forwards the tracer's request to
//! one Datadog endpoint right away, adds the API key it resolved, and returns
//! Datadog's reply unchanged. This lets tracers reach Datadog when only the
//! Extension can read the key (for example `DD_API_KEY_SECRET_ARN`).

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{RawQuery, State},
    http::{HeaderMap, HeaderName, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use dogstatsd::api_key::ApiKeyFactory;
use tracing::{debug, error};

use crate::config::Config;

/// Feature flag configuration (rules) used by the tracers' flag providers.
pub const FLAG_CONFIG_PATH: &str = "/api/v2/feature-flagging/config/rules-based/server";
const FLAG_CONFIG_SUBDOMAIN: &str = "ufc-server.ff-cdn";

/// Below the tracers' default 5 second request timeout, so the tracer sees
/// our error instead of timing out itself.
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
    client: reqwest::Client,
    flag_config_base_url: String,
    env: Option<String>,
}

impl Passthrough {
    #[must_use]
    pub fn new(
        config: &Config,
        api_key_factory: Arc<ApiKeyFactory>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            api_key_factory,
            client,
            flag_config_base_url: format!("https://{FLAG_CONFIG_SUBDOMAIN}.{}", config.site),
            env: config.env.clone(),
        }
    }

    pub fn router(self) -> Router {
        Router::new()
            .route(FLAG_CONFIG_PATH, get(flag_config))
            .with_state(self)
    }
}

async fn flag_config(
    State(passthrough): State<Passthrough>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    // 403 makes the flag provider log the failure without retrying; a 5xx
    // would be retried with backoff.
    let Some(api_key) = passthrough.api_key_factory.get_api_key().await else {
        error!("TRACE_AGENT | Failed to resolve API key, can't forward the flag config request");
        return (StatusCode::FORBIDDEN, "Datadog API key unavailable").into_response();
    };

    let mut url = format!("{}{FLAG_CONFIG_PATH}", passthrough.flag_config_base_url);
    if let Some(query) = &query {
        url.push('?');
        url.push_str(query);
    }

    let mut request = passthrough
        .client
        .get(url)
        .timeout(UPSTREAM_TIMEOUT)
        .header("DD-API-KEY", api_key);
    // The tracer drops `dd_env` when it uses a custom base URL.
    let has_env = query.as_deref().is_some_and(|q| {
        q.split('&')
            .any(|pair| pair == "dd_env" || pair.starts_with("dd_env="))
    });
    if let (Some(env), false) = (&passthrough.env, has_env) {
        request = request.query(&[("dd_env", env)]);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use httpmock::prelude::*;
    use tower::ServiceExt;

    fn passthrough(base_url: String, api_key_factory: ApiKeyFactory) -> Passthrough {
        Passthrough {
            api_key_factory: Arc::new(api_key_factory),
            client: reqwest::Client::new(),
            flag_config_base_url: base_url,
            env: Some("prod".to_string()),
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
