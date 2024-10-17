use dogstatsd::{aggregator::Aggregator, metric::{Metric, Type}};

use super::constants;
use crate::proc::proc::{self, NetworkData};
use tracing::debug;
use tracing::error;
use std::sync::MutexGuard;

pub trait EnhancedMetricData {
    fn send_metrics(&self, aggr: &mut MutexGuard<Aggregator>);
}

#[derive(Copy, Clone)]
pub struct NetworkEnhancedMetricData {
    pub offset: NetworkData
}

impl NetworkEnhancedMetricData {
    pub fn generate_metrics(&self, rx_bytes_end: f64, tx_bytes_end: f64, aggr: &mut MutexGuard<Aggregator>) {
        let rx_bytes = rx_bytes_end - self.offset.rx_bytes;
        let tx_bytes = tx_bytes_end - self.offset.tx_bytes;

        let metric = Metric::new(
            constants::RX_BYTES_METRIC.into(),
            Type::Distribution,
            rx_bytes.to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert rx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TX_BYTES_METRIC.into(),
            Type::Distribution,
            tx_bytes.to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert tx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TOTAL_NETWORK_METRIC.into(),
            Type::Distribution,
            (rx_bytes + tx_bytes).to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
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
