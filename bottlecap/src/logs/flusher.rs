use crate::config;
use crate::logs::{aggregator::Aggregator, datadog};
use std::sync::{Arc, Mutex};
use tokio::task::JoinSet;
use tracing::{debug, error};

pub struct Flusher {
    dd_api: datadog::Api,
    api_key: String,
    site: String,
    aggregator: Arc<Mutex<Aggregator>>,
}
#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(config: Arc<config::Config>, aggregator: Arc<Mutex<Aggregator>>) -> Self {
        let dd_api = datadog::Api::new(config.api_key.clone(), config.site.clone());
        Flusher { dd_api, site: config.site.clone(), api_key: config.api_key.clone(), aggregator }
    }
    pub async fn flush(&self) {
        let mut guard = self.aggregator.lock().expect("lock poisoned");
        let mut set = JoinSet::new();
        // It could be an empty JSON array: []
        let mut logs = guard.get_batch();
        while logs.len() > 2 {
            let _api_key = self.api_key.clone();
            let _site = self.site.clone();
            set.spawn(async move {
                self.send_dd(logs).await
            });
            // set.spawn(async move {
            //     Self::send(reqwest::Client::new(), api_key, site, logs).await
            // });

            

            // if let Err(e) = self.dd_api
            //     .send(logs)
            //     .await {
            //     debug!("Failed to send logs to datadog: {}", e);
            // }
            logs = guard.get_batch();
        }
        drop(guard);
        while let Some(res) = set.join_next().await {
            if let Err(e) = res {
                debug!("Failed to send logs to datadog: {}", e);
            }
        }
    }

    async fn send_dd(&self, logs: Vec<u8>) -> Result<(), String>{
        self.dd_api.send(logs).await
    }

    pub async fn flush_shutdown(aggregator: &Arc<Mutex<Aggregator>>, dd_api: &datadog::Api) {
        let mut aggregator = aggregator.lock().expect("lock poisoned");
        let mut logs = aggregator.get_batch();
        // It could be an empty JSON array: []
        while logs.len() > 2 {
            dd_api
                .send(logs)
                .await
                .expect("Failed to send logs to Datadog");
            logs = aggregator.get_batch();
        }
    }
    async fn send(client: reqwest::Client, api_key: String, site: String, data: Vec<u8>) -> Result<(), String> {
        let url = format!("https://http-intake.logs.{}/api/v2/logs", site);

        // It could be an empty JSON array: []
        if data.len() > 2 {
            let resp: Result<reqwest::Response, reqwest::Error> = client
                .post(&url)
                .header("DD-API-KEY", api_key)
                .header("DD-PROTOCOL", "agent-json")
                .header("Content-Type", "application/json")
                .body(data)
                .send()
                .await;

            match resp {
                Ok(resp) => {
                    if resp.status() != 202 {
                        debug!("Failed to send logs to datadog: {}", resp.status());
                    }
                }
                Err(e) => {
                    error!("Failed to send logs to datadog: {}", e);
                }
            }
        }

        Ok(())
    }
}
