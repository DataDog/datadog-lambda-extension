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
    pub fn generate_metrics(&self, rx_bytes: f64, tx_bytes: f64, aggr: &mut MutexGuard<Aggregator>) {
        let adjusted_rx_bytes = rx_bytes - self.offset.rx_bytes;
        let adjusted_tx_bytes = tx_bytes - self.offset.tx_bytes;

        let metric = Metric::new(
            constants::RX_BYTES_METRIC.into(),
            Type::Distribution,
            adjusted_rx_bytes.to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert rx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TX_BYTES_METRIC.into(),
            Type::Distribution,
            adjusted_tx_bytes.to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert tx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TOTAL_NETWORK_METRIC.into(),
            Type::Distribution,
            (adjusted_rx_bytes + adjusted_tx_bytes).to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert total_network metric: {}", e);
        }
    }
}

impl EnhancedMetricData for NetworkEnhancedMetricData {
    fn send_metrics(&self, aggr: &mut MutexGuard<Aggregator>) {
        let data_result = proc::get_network_data();
        if data_result.is_err() {
            debug!("Could not emit network enhanced metrics");
            return; 
        }
        let data = data_result.unwrap();

        self.generate_metrics(data.rx_bytes, data.tx_bytes, aggr);
    }
}
