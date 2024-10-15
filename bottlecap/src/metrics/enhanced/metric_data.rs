use dogstatsd::metric::{Metric, Type};

use super::constants;
use crate::proc::proc::{self, NetworkData};
use tracing::debug;

pub trait EnhancedMetricData {
    fn get_metrics(&self) -> Vec<Metric>;
}

#[derive(Copy, Clone)]
pub struct NetworkEnhancedMetricData {
    pub offset: NetworkData
}

impl EnhancedMetricData for NetworkEnhancedMetricData {
    fn get_metrics(&self) -> Vec<Metric> {
        debug!("=== entered NetworkEnhancedMetricData get_metrics fn ===");
        let mut metrics: Vec<Metric> = Vec::new();

        let data_result = proc::get_network_data();
        if data_result.is_err() {
            debug!("Could not emit network enhanced metrics");
            return metrics; 
        }
        let data = data_result.unwrap();

        let rx_bytes = data.rx_bytes - self.offset.rx_bytes;
        let tx_bytes = data.tx_bytes - self.offset.tx_bytes;

        let metric = Metric::new(
            constants::RX_BYTES_METRIC.into(),
            Type::Distribution,
            rx_bytes.to_string().into(),
            None,
        );
        metrics.push(metric);


        let metric = Metric::new(
            constants::TX_BYTES_METRIC.into(),
            Type::Distribution,
            tx_bytes.to_string().into(),
            None,
        );
        metrics.push(metric);

        let metric = Metric::new(
            constants::TOTAL_NETWORK_METRIC.into(),
            Type::Distribution,
            (rx_bytes + tx_bytes).to_string().into(),
            None,
        );
        metrics.push(metric);
        debug!("=== returning network metrics: {:?} ===", metrics);

        return metrics;
    }
}
