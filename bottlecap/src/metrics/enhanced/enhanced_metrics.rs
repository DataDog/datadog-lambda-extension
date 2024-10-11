// use super::constants::{RX_BYTES_METRIC, TX_BYTES_METRIC};
// use crate::proc::proc::NetworkEnhancedMetricData;

// trait EnhancedMetricData {
//     fn set_metric_data(&mut self, name: String, value: f64) -> ();
//     fn get_metric_data(&self, name: String) -> Metric;
// }

// impl EnhancedMetricData for NetworkEnhancedMetricData {
//     fn set_metric_data(&mut self, name: String, value: f64) -> () {
//         match name.as_str() {
//             RX_BYTES_METRIC => self.rx_bytes = value,
//             TX_BYTES_METRIC => self.tx_bytes = value,
//             _ => None,
//         }
//     }

//     fn get_metric_data(&self, name: String) -> Metric {
//         match name.as_str() {
//             RX_BYTES_METRIC => return self.rx_bytes,
//             TX_BYTES_METRIC => return self.tx_bytes,
//             _ => None,
//         }
//     }
// }