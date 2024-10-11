use std::{error::Error, fs::File, io::{self, BufRead}};

use super::constants::{LAMDBA_NETWORK_INTERFACE, PROC_NET_DEV_PATH};

#[derive(Copy, Clone)]
pub struct NetworkEnhancedMetricData {
    pub rx_bytes: f64,
    pub tx_bytes: f64,
}

pub fn get_network_data() -> Result<NetworkEnhancedMetricData, Box<dyn std::error::Error>> {
    get_network_data_helper(PROC_NET_DEV_PATH)
}

fn get_network_data_helper(path: &str) -> Result<NetworkEnhancedMetricData, Box<dyn std::error::Error>> {
    let file = File::open(path).map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let reader = io::BufReader::new(file);

    for line in reader.lines() {
        let line = line.map_err(|e| Box::new(e) as Box<dyn Error>)?;
        let mut values = line.split_whitespace();

        let interface_name = match values.next() {
            Some(name) => name,
            None => continue,
        };
        if !interface_name.starts_with(LAMDBA_NETWORK_INTERFACE) {
            continue;
        }

        let rx_bytes: f64 = match values.next().and_then(|s| s.parse().ok()) {
            Some(value) => value,
            None => continue,
        };
        let tx_bytes: f64 = match values.nth(7).and_then(|s| s.parse().ok()) {
            Some(value) => value,
            None => continue,
        };

        return Ok(NetworkEnhancedMetricData { rx_bytes, tx_bytes });
    }

    Err("No matching network interface found".into())
}