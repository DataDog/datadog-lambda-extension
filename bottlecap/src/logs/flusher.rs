use crate::FLUSH_RETRY_COUNT;
use crate::config;
use crate::http::get_client;
use crate::logs::aggregator::Aggregator;
use dogstatsd::api_key::ApiKeyFactory;
use futures::future::join_all;
use hyper::StatusCode;
use reqwest::header::HeaderMap;
use std::error::Error;
use std::time::Instant;
use std::{io::Write, sync::Arc};
use thiserror::Error as ThisError;
use tokio::{
    sync::{Mutex, OnceCell},
    task::JoinSet,
};
use tracing::{debug, error};
use zstd::stream::write::Encoder;

#[derive(ThisError, Debug)]
#[error("{message}")]
pub struct FailedRequestError {
    pub request: reqwest::RequestBuilder,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Flusher {
    client: reqwest::Client,
    endpoint: String,
    aggregator: Arc<Mutex<Aggregator>>,
    config: Arc<config::Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    headers: OnceCell<HeaderMap>,
}

impl Flusher {
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        endpoint: String,
        aggregator: Arc<Mutex<Aggregator>>,
        config: Arc<config::Config>,
    ) -> Self {
        let client = get_client(&config);
        Flusher {
            client,
            endpoint,
            aggregator,
            config,
            api_key_factory,
            headers: OnceCell::new(),
        }
    }

    pub async fn flush(&self, batches: Option<Arc<Vec<Vec<u8>>>>) -> Vec<reqwest::RequestBuilder> {
        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!("Skipping flushing logs: Failed to resolve API key");
            return vec![];
        };

        let mut set = JoinSet::new();

        if let Some(logs_batches) = batches {
            for batch in logs_batches.iter() {
                if batch.is_empty() {
                    continue;
                }
                let req = self.create_request(batch.clone(), api_key).await;
                set.spawn(async move { Self::send(req).await });
            }
        }

        let mut failed_requests = Vec::new();
        for result in set.join_all().await {
            if let Err(e) = result {
                debug!("Failed to join task: {}", e);
                continue;
            }

            // At this point we know the task completed successfully,
            // but the send operation itself may have failed
            if let Err(e) = result {
                if let Some(failed_req_err) = e.downcast_ref::<FailedRequestError>() {
                    // Clone the request from our custom error
                    failed_requests.push(
                        failed_req_err
                            .request
                            .try_clone()
                            .expect("should be able to clone request"),
                    );
                    debug!("Failed to send logs after retries, will retry later");
                } else {
                    debug!("Failed to send logs: {}", e);
                }
            }
        }
        failed_requests
    }

    async fn create_request(&self, data: Vec<u8>, api_key: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/api/v2/logs", self.endpoint);
        let headers = self.get_headers(api_key).await;
        self.client
            .post(&url)
            .timeout(std::time::Duration::from_secs(self.config.flush_timeout))
            .headers(headers.clone())
            .body(data)
    }

    async fn send(req: reqwest::RequestBuilder) -> Result<(), Box<dyn Error + Send>> {
        let mut attempts = 0;

        loop {
            let time = Instant::now();
            attempts += 1;
            let Some(cloned_req) = req.try_clone() else {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "can't clone",
                )));
            };
            let resp = cloned_req.send().await;
            let elapsed = time.elapsed();

            match resp {
                Ok(resp) => {
                    let status = resp.status();
                    _ = resp.text().await;
                    if status == StatusCode::FORBIDDEN {
                        // Access denied. Stop retrying.
                        error!(
                            "Failed to send logs to Datadog: Access denied. Please verify that your API key is valid."
                        );
                        return Ok(());
                    }
                    if status.is_success() {
                        return Ok(());
                    }
                }
                Err(e) => {
                    if attempts >= FLUSH_RETRY_COUNT {
                        // After 3 failed attempts, return the original request for later retry
                        // Create a custom error that can be downcast to get the RequestBuilder
                        error!(
                            "Failed to send logs to datadog after {} ms and {} attempts: {:?}",
                            elapsed.as_millis(),
                            attempts,
                            e
                        );
                        return Err(Box::new(FailedRequestError {
                            request: req,
                            message: format!("Failed after {attempts} attempts: {e}"),
                        }));
                    }
                }
            }
        }
    }

    async fn get_headers(&self, api_key: &str) -> &HeaderMap {
        self.headers
            .get_or_init(move || async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    "DD-API-KEY",
                    api_key.parse().expect("failed to parse header"),
                );
                if !self.config.enable_observability_pipeline_forwarding {
                    headers.insert(
                        "DD-PROTOCOL",
                        "agent-json".parse().expect("failed to parse header"),
                    );
                }
                headers.insert(
                    "Content-Type",
                    "application/json".parse().expect("failed to parse header"),
                );

                if self.config.logs_config_use_compression
                    && !self.config.enable_observability_pipeline_forwarding
                {
                    headers.insert(
                        "Content-Encoding",
                        "zstd".parse().expect("failed to parse header"),
                    );
                }
                headers
            })
            .await
    }
}

#[allow(clippy::module_name_repetitions)]
#[derive(Clone)]
pub struct LogsFlusher {
    config: Arc<config::Config>,
    pub flushers: Vec<Flusher>,
}

impl LogsFlusher {
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator: Arc<Mutex<Aggregator>>,
        config: Arc<config::Config>,
    ) -> Self {
        let mut flushers = Vec::new();

        // Create primary flusher
        flushers.push(Flusher::new(
            Arc::clone(&api_key_factory),
            config.logs_config_logs_dd_url.clone(),
            aggregator.clone(),
            config.clone(),
        ));

        // Create flushers for additional endpoints
        for endpoint in &config.logs_config_additional_endpoints {
            let endpoint_url = format!("https://{}:{}", endpoint.host, endpoint.port);
            flushers.push(Flusher::new(
                Arc::clone(&api_key_factory),
                endpoint_url,
                aggregator.clone(),
                config.clone(),
            ));
        }

        LogsFlusher { config, flushers }
    }

    pub async fn flush(
        &self,
        retry_request: Option<reqwest::RequestBuilder>,
    ) -> Vec<reqwest::RequestBuilder> {
        let mut failed_requests = Vec::new();

        // If retry_request is provided, only process that request
        if let Some(req) = retry_request {
            if let Some(req_clone) = req.try_clone() {
                if let Err(e) = Flusher::send(req_clone).await {
                    if let Some(failed_req_err) = e.downcast_ref::<FailedRequestError>() {
                        failed_requests.push(
                            failed_req_err
                                .request
                                .try_clone()
                                .expect("should be able to clone request"),
                        );
                    }
                }
            }
        } else {
            // Get batches from primary flusher's aggregator
            let logs_batches = Arc::new({
                let mut guard = self.flushers[0].aggregator.lock().await;
                let mut batches = Vec::new();
                let mut current_batch = guard.get_batch();
                while !current_batch.is_empty() {
                    let transformed = Self::to_flat_json(current_batch);
                    batches.push(self.compress(transformed));
                    current_batch = guard.get_batch();
                }
                batches
            });

            // Send batches to each flusher
            let futures = self.flushers.iter().map(|flusher| {
                let batches = Arc::clone(&logs_batches);
                let flusher = flusher.clone();
                async move { flusher.flush(Some(batches)).await }
            });

            let results = join_all(futures).await;
            for failed in results {
                failed_requests.extend(failed);
            }
        }
        failed_requests
    }

    fn compress(&self, data: Vec<u8>) -> Vec<u8> {
        if !self.config.logs_config_use_compression
            || self.config.enable_observability_pipeline_forwarding
        {
            return data;
        }

        match self.encode(&data) {
            Ok(compressed_data) => compressed_data,
            Err(e) => {
                debug!("Failed to compress data: {}", e);
                data
            }
        }
    }

    fn encode(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut encoder = Encoder::new(Vec::new(), self.config.logs_config_compression_level)?;
        encoder.write_all(data)?;
        encoder.finish().map_err(|e| Box::new(e) as Box<dyn Error>)
    }

    /*
    Transform aggregated IntakeLog array into Datadog v2 flat JSON array.
    This is needed for sending logs to an Observability Pipeline.
    Before:
    {
        "ddsource": "lambda",
        "message": {
            "message": "START RequestId: f301b84e-b59a-4f35-834a-aaded297f6b0 Version: $LATEST",
            "status": "info",
            "timestamp": 1757352915141
        }
    }
    After:
    {
        "ddsource": "lambda",
        "message": "START RequestId: f301b84e-b59a-4f35-834a-aaded297f6b0 Version: $LATEST",
        "status": "info",
        "timestamp": 1757352915141
    }
     */
    pub(crate) fn to_flat_json(data: Vec<u8>) -> Vec<u8> {
        let _input_bytes = data.len();
        let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&data);
        let Ok(serde_json::Value::Array(items)) = parsed else {
            return data;
        };
        let total_items = items.len();
        let mut out = Vec::with_capacity(total_items);
        for v in items {
            let message = match v.get("message") {
                Some(serde_json::Value::Object(m)) => m
                    .get("message")
                    .map(|inner| match inner {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let status = v
                .get("message")
                .and_then(|m| m.get("status"))
                .and_then(|m| m.as_str())
                .or_else(|| v.get("status").and_then(|m| m.as_str()))
                .unwrap_or("info")
                .to_string();
            let timestamp = v
                .get("message")
                .and_then(|m| m.get("timestamp"))
                .and_then(serde_json::Value::as_i64)
                .or_else(|| v.get("timestamp").and_then(serde_json::Value::as_i64))
                .unwrap_or(0);
            let ddsource = v
                .get("ddsource")
                .or_else(|| v.get("source"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let service = v
                .get("service")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let hostname = v
                .get("hostname")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let ddtags = v
                .get("ddtags")
                .or_else(|| v.get("tags"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let mut obj = serde_json::Map::new();
            obj.insert("message".to_string(), serde_json::Value::String(message));
            obj.insert("status".to_string(), serde_json::Value::String(status));
            obj.insert("timestamp".to_string(), serde_json::Value::from(timestamp));
            obj.insert("ddsource".to_string(), serde_json::Value::String(ddsource));
            obj.insert("service".to_string(), serde_json::Value::String(service));
            obj.insert("hostname".to_string(), serde_json::Value::String(hostname));
            obj.insert("ddtags".to_string(), serde_json::Value::String(ddtags));
            out.push(serde_json::Value::Object(obj));
        }
        let _out_len = out.len();
        serde_json::to_vec(&serde_json::Value::Array(out)).unwrap_or(data)
    }
}

#[cfg(test)]
mod tests {
    use super::LogsFlusher;
    use serde_json::json;

    #[test]
    fn test_to_flat_json() {
        let batch = json!([
            {
                "ddsource": "lambda",
                "service": "svc",
                "hostname": "host",
                "ddtags": "env:dev",
                "message": {
                    "message": "hello",
                    "status": "info",
                    "timestamp": 1234
                }
            },
            {
                "message": "flat",
                "status": "debug",
                "timestamp": 5678,
                "ddsource": "lambda"
            }
        ]);
        let out = LogsFlusher::to_flat_json(
            serde_json::to_vec(&batch).expect("Failed to serialize test batch"),
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out).expect("Failed to deserialize result");
        assert!(v.is_array());
        let item = &v[0];
        assert_eq!(item.get("ddsource").expect("test field missing"), "lambda");
        assert_eq!(item.get("service").expect("test field missing"), "svc");
        assert_eq!(item.get("hostname").expect("test field missing"), "host");
        assert_eq!(item.get("ddtags").expect("test field missing"), "env:dev");
        assert_eq!(item.get("message").expect("test field missing"), "hello");
        assert_eq!(item.get("status").expect("test field missing"), "info");
        assert_eq!(item.get("timestamp").expect("test field missing"), 1234);
        let item = &v[1];
        assert_eq!(item.get("message").expect("test field missing"), "flat");
        assert_eq!(item.get("status").expect("test field missing"), "debug");
        assert_eq!(item.get("timestamp").expect("test field missing"), 5678);
        assert_eq!(item.get("ddsource").expect("test field missing"), "lambda");
    }
}
