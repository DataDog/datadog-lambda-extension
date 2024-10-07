use std::{error::Error, fs::File, io::{self, BufRead}};

use super::constants;

#[derive(Copy, Clone)]
pub struct NetworkData {
    pub rx_bytes: f64,
    pub tx_bytes: f64,
}

pub fn get_network_data() -> Result<NetworkData, Box<dyn std::error::Error>> {
    return get_network_data_helper(constants::PROC_NET_DEV_PATH);
}

fn get_network_data_helper(path: &str) -> Result<NetworkData, Box<dyn std::error::Error>> {
    println!("=== entered getNetworkDataHelper ===");

    let file = File::open(path).map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let reader = io::BufReader::new(file);

    for line in reader.lines() {
        let line = line.map_err(|e| Box::new(e) as Box<dyn Error>)?;
        println!("{}", line);

        let mut values = line.split_whitespace();

        let interface_name = match values.next() {
            Some(name) => name,
            None => continue,
        };

        let rx_bytes: f64 = match values.next().and_then(|s| s.parse().ok()) {
            Some(value) => value,
            None => continue,
        };

        let tx_bytes: f64 = match values.nth(7).and_then(|s| s.parse().ok()) {
            Some(value) => value,
            None => continue,
        };

        if interface_name.starts_with(constants::LAMDBA_NETWORK_INTERFACE) {
            return Ok(NetworkData { rx_bytes, tx_bytes });
        }
    }

    Err("No matching network interface found".into())
}