use std::str::Split;
use tracing::{debug, error};

use crate::metrics::aggregator::Aggregator;
use crate::metrics::metric::Metric;
use std::sync::{Arc, Mutex};

pub struct DogStatsD {
    cancel_token: tokio_util::sync::CancellationToken,
    aggregator: Arc<Mutex<Aggregator>>,
    socket: tokio::net::UdpSocket,
}

pub struct DogStatsDConfig {
    pub host: String,
    pub port: u16,
}

impl DogStatsD {
    #[must_use]
    pub async fn new(
        config: &DogStatsDConfig,
        aggregator: Arc<Mutex<Aggregator>>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> DogStatsD {
        let addr = format!("{}:{}", config.host, config.port);
        // TODO (UDS socket)
        let socket = tokio::net::UdpSocket::bind(addr)
            .await
            .expect("couldn't bind to address");
        DogStatsD {
            cancel_token,
            aggregator,
            socket,
        }
    }

    pub async fn spin(self) {
        let mut spin_cancelled = false;
        while !spin_cancelled {
            // TODO(astuyve) this should be dynamic
            let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
            let (amt, src) = self
                .socket
                .recv_from(&mut buf)
                .await
                .expect("didn't receive data");
            let buf = &mut buf[..amt];
            let msgs = std::str::from_utf8(buf).expect("couldn't parse as string");
            debug!(
                "received message: {} from {}, sending it to the bus",
                msgs, src
            );
            let statsd_metric_strings = msgs.split('\n');
            self.insert_metrics(statsd_metric_strings);
            spin_cancelled = self.cancel_token.is_cancelled();
        }
    }

    fn insert_metrics(&self, msg: Split<char>) {
        let all_valid_metrics: Vec<Metric> = msg
            .filter_map(|m| match Metric::parse(m) {
                Ok(metric) => Some(metric),
                Err(e) => {
                    error!("failed to parse metric: {}", e);
                    None
                }
            })
            .collect();
        if !all_valid_metrics.is_empty() {
            let mut guarded_aggregator = self.aggregator.lock().expect("lock poisoned");
            for a_valid_value in all_valid_metrics {
                let _ = guarded_aggregator.insert(&a_valid_value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // use std::sync::{Arc, Mutex};
    // use crate::metrics::aggregator::Aggregator;
    // use crate::metrics::dogstatsd::{DogStatsD, DogStatsDConfig};
    // use crate::tags::provider::Provider;

    // #[tokio::test]
    // async fn test_dogstatsd() {
    //     let aggregator = Arc::new(Mutex::new(
    //         Aggregator::new(Arc::new(Provider::new(
    //             Arc::new(Default::default()), "".to_string(), &Default::default()), ), 1_024));
    //     let cancel_token = tokio_util::sync::CancellationToken::new();
    //     let dogstatsd = DogStatsD::new(&DogStatsDConfig {
    //         host: "".to_string(),
    //         port: 0,
    //     }, aggregator, Default::default());
    // }
}
