use super::constants;
use crate::proc::proc::{self, NetworkData};
use dogstatsd::{
    aggregator::Aggregator,
    metric::{Metric, MetricValue},
};
use std::sync::MutexGuard;
use tracing::debug;
use tracing::error;

#[allow(clippy::module_name_repetitions)]
pub trait EnhancedMetricData {
    fn send_metrics(&self, aggr: &mut MutexGuard<Aggregator>);
}

#[allow(clippy::module_name_repetitions)]
#[derive(Copy, Clone)]
pub struct NetworkEnhancedMetricData {
    pub offset: NetworkData,
}

impl NetworkEnhancedMetricData {
    pub fn generate_metrics(
        &self,
        rx_bytes_end: f64,
        tx_bytes_end: f64,
        aggr: &mut MutexGuard<Aggregator>,
    ) {
        let rx_bytes = rx_bytes_end - self.offset.rx_bytes;
        let tx_bytes = tx_bytes_end - self.offset.tx_bytes;
        let total_network = rx_bytes + tx_bytes;

        let metric = Metric::new(
            constants::RX_BYTES_METRIC.into(),
            MetricValue::distribution(rx_bytes),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert rx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TX_BYTES_METRIC.into(),
            MetricValue::distribution(tx_bytes),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert tx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TOTAL_NETWORK_METRIC.into(),
            MetricValue::distribution(total_network),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert total_network metric: {}", e);
        }
    }
}

impl EnhancedMetricData for NetworkEnhancedMetricData {
    fn send_metrics(&self, aggr: &mut MutexGuard<Aggregator>) {
        match proc::get_network_data() {
            Ok(data) => {
                self.generate_metrics(data.rx_bytes, data.tx_bytes, aggr);
            }
            Err(_e) => {
                debug!("Could not emit network enhanced metrics");
            }
        }
    }
}
