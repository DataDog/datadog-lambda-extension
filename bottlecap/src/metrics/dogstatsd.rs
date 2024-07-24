use std::net::SocketAddr;
use std::str::Split;
use std::sync::{Arc, Mutex};

use tracing::{debug, error};

use crate::metrics::aggregator::Aggregator;
use crate::metrics::metric::Metric;

pub struct DogStatsD {
    cancel_token: tokio_util::sync::CancellationToken,
    aggregator: Arc<Mutex<Aggregator>>,
    buffer_reader: BufferReader,
}

pub struct DogStatsDConfig {
    pub host: String,
    pub port: u16,
}

enum BufferReader {
    UdpSocketReader(tokio::net::UdpSocket),
    #[allow(dead_code)]
    MirrorReader(Vec<u8>, SocketAddr),
}

impl BufferReader {
    async fn read(&self) -> std::io::Result<(Vec<u8>, SocketAddr)> {
        match self {
            BufferReader::UdpSocketReader(socket) => {
                // TODO(astuyve) this should be dynamic
                let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
                let (amt, src) = socket
                    .recv_from(&mut buf)
                    .await
                    .expect("didn't receive data");
                Ok((buf[..amt].to_owned(), src))
            }
            BufferReader::MirrorReader(data, socket) => Ok((data.clone(), *socket)),
        }
    }
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
            buffer_reader: BufferReader::UdpSocketReader(socket),
        }
    }

    pub async fn spin(self) {
        let mut spin_cancelled = false;
        while !spin_cancelled {
            self.consume_statsd().await;
            spin_cancelled = self.cancel_token.is_cancelled();
        }
    }

    async fn consume_statsd(&self) {
        let (buf, src) = self
            .buffer_reader
            .read()
            .await
            .expect("didn't receive data");
        let msgs = std::str::from_utf8(&buf).expect("couldn't parse as string");
        debug!("Received message: {} from {}", msgs, src);
        let statsd_metric_strings = msgs.split('\n');
        self.insert_metrics(statsd_metric_strings);
    }

    fn insert_metrics(&self, msg: Split<char>) {
        let all_valid_metrics: Vec<Metric> = msg
            .filter_map(|m| match Metric::parse(m) {
                Ok(metric) => Some(metric),
                Err(e) => {
                    error!("Failed to parse metric {}: {}", m, e);
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
    use crate::config::Config;
    use crate::metrics::aggregator::Aggregator;
    use crate::metrics::aggregator::ValueVariant::{DDSketch, Value};
    use crate::metrics::dogstatsd::{BufferReader, DogStatsD};
    use crate::tags::provider::Provider;
    use crate::LAMBDA_RUNTIME_SLUG;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn test_dogstatsd_multi_distribution() {
        let precision = 0.000_000_01;
        let locked_aggregator = setup_dogstatsd(
            "single_machine_performance.rouster.api.series_v2.payload_size_bytes:269942|d
single_machine_performance.rouster.metrics_min_timestamp_latency:1426.90870216|d
single_machine_performance.rouster.metrics_max_timestamp_latency:1376.90870216|d
",
        )
        .await;
        let mut aggregator = locked_aggregator.lock().expect("lock poisoned");

        let parsed_metrics = aggregator.distributions_to_protobuf();

        assert_eq!(parsed_metrics.sketches.len(), 3);
        assert_eq!(aggregator.to_series().len(), 0);

        match aggregator.get_value_by_id(
            "single_machine_performance.rouster.api.series_v2.payload_size_bytes".into(),
            None,
        ) {
            Some(DDSketch(metric)) => {
                assert!((metric.max().unwrap() - 269942f64).abs() < precision);
                assert!((metric.min().unwrap() - 269942f64).abs() < precision);
                assert!((metric.sum().unwrap() - 269942f64).abs() < precision);
                assert!((metric.avg().unwrap() - 269942f64).abs() < precision);
            }
            _ => panic!(
                "single_machine_performance.rouster.api.series_v2.payload_size_bytes not found"
            ),
        }

        match aggregator.get_value_by_id(
            "single_machine_performance.rouster.metrics_min_timestamp_latency".into(),
            None,
        ) {
            Some(DDSketch(metric)) => {
                assert!((metric.max().unwrap() - 1426.90870216).abs() < precision);
                assert!((metric.min().unwrap() - 1426.90870216).abs() < precision);
                assert!((metric.sum().unwrap() - 1426.90870216).abs() < precision);
                assert!((metric.avg().unwrap() - 1426.90870216).abs() < precision);
            }
            _ => {
                panic!("single_machine_performance.rouster.metrics_min_timestamp_latency not found")
            }
        }

        match aggregator.get_value_by_id(
            "single_machine_performance.rouster.metrics_max_timestamp_latency".into(),
            None,
        ) {
            Some(DDSketch(metric)) => {
                assert!((metric.max().unwrap() - 1376.90870216).abs() < precision);
                assert!((metric.min().unwrap() - 1376.90870216).abs() < precision);
                assert!((metric.sum().unwrap() - 1376.90870216).abs() < precision);
                assert!((metric.avg().unwrap() - 1376.90870216).abs() < precision);
            }
            _ => {
                panic!("single_machine_performance.rouster.metrics_max_timestamp_latency not found")
            }
        }
    }

    #[tokio::test]
    async fn test_dogstatsd_multi_metric() {
        let precision = 0.000_000_01;
        let locked_aggregator = setup_dogstatsd(
            "metric1:1|c\nmetric2:2|c|tag2:val2\nmetric3:3|c||tag3:val3,tag4:val4\n",
        )
        .await;
        let mut aggregator = locked_aggregator.lock().expect("lock poisoned");

        let parsed_metrics = aggregator.to_series();

        assert_eq!(parsed_metrics.len(), 3);
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 0);

        match aggregator.get_value_by_id("metric1".into(), None) {
            Some(Value(metric)) => {
                assert!((metric - 1.0).abs() < precision);
            }
            _ => panic!("metric1 not found"),
        }

        match aggregator.get_value_by_id("metric2".into(), None) {
            Some(Value(metric)) => {
                assert!((metric - 2.0).abs() < precision);
            }
            _ => panic!("metric2 not found"),
        }

        match aggregator.get_value_by_id("metric3".into(), None) {
            Some(Value(metric)) => {
                assert!((metric - 3.0).abs() < precision);
            }
            _ => panic!("metric3 not found"),
        }
    }

    #[tokio::test]
    async fn test_dogstatsd_single_metric() {
        let precision = 0.000_000_01;
        let locked_aggregator = setup_dogstatsd("metric123:99123|c").await;
        let mut aggregator = locked_aggregator.lock().expect("lock poisoned");
        let parsed_metrics = aggregator.to_series();

        assert_eq!(parsed_metrics.len(), 1);
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 0);

        match aggregator.get_value_by_id("metric123".into(), None) {
            Some(Value(metric)) => {
                assert!((metric - 99_123.0).abs() < precision);
            }
            _ => panic!("metric1 not found"),
        }
    }

    #[tokio::test]
    async fn invalid_dogstatsd_no_panic() {
        let locked_aggregator = setup_dogstatsd("somerandomstring|c+a;slda").await;
        let aggregator = locked_aggregator.lock().expect("lock poisoned");
        let parsed_metrics = aggregator.to_series();

        assert_eq!(parsed_metrics.len(), 0);
    }

    async fn setup_dogstatsd(statsd_string: &str) -> Arc<Mutex<Aggregator>> {
        let tags_provider = Arc::new(Provider::new(
            Arc::new(Config::default()),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::default(),
        ));
        let aggregator_arc = Arc::new(Mutex::new(
            Aggregator::new(tags_provider, 1_024).expect("aggregator creation failed"),
        ));
        let cancel_token = tokio_util::sync::CancellationToken::new();

        let dogstatsd = DogStatsD {
            cancel_token,
            aggregator: Arc::clone(&aggregator_arc),
            buffer_reader: BufferReader::MirrorReader(
                statsd_string.as_bytes().to_vec(),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(111, 112, 113, 114)), 0),
            ),
        };
        dogstatsd.consume_statsd().await;

        aggregator_arc
    }
}
