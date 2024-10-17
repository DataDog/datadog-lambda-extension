use std::{error::Error, fs::File, io::{self, BufRead}};

use super::constants::{LAMDBA_NETWORK_INTERFACE, PROC_NET_DEV_PATH};

#[derive(Copy, Clone)]
pub struct NetworkData {
    pub rx_bytes: f64,
    pub tx_bytes: f64,
}

pub fn get_network_data() -> Result<NetworkData, Box<dyn std::error::Error>> {
    get_network_data_helper(PROC_NET_DEV_PATH)
}

fn get_network_data_helper(path: &str) -> Result<NetworkData, Box<dyn std::error::Error>> {
    let file = File::open(path).map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let reader = io::BufReader::new(file);

    for line in reader.lines() {
        let line = line.map_err(|e| Box::new(e) as Box<dyn Error>)?;
        let mut values = line.split_whitespace();

        let Some(interface_name) = values.next() else { continue };
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

        return Ok(NetworkData { rx_bytes, tx_bytes });
    }

    Err("No matching network interface found".into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_network_data() {
        let path = "./src/proc/test_data/net/valid_dev";
        let network_data_result = get_network_data_helper(&path);
        assert!(!network_data_result.is_err());
        let network_data_result = network_data_result.unwrap();
        assert_eq!(network_data_result.rx_bytes, 180.0);
        assert_eq!(network_data_result.tx_bytes, 254.0);

        let path = "./src/proc/test_data/net/invalid_dev_malformed";
        let network_data_result = get_network_data_helper(&path);
        assert!(network_data_result.is_err());

        let path = "./src/proc/test_data/net/invalid_dev_non_numerical_value";
        let network_data_result = get_network_data_helper(&path);
        assert!(network_data_result.is_err());

        let path = "./src/proc/test_data/net/missing_interface_dev";
        let network_data_result = get_network_data_helper(&path);
        assert!(network_data_result.is_err());

        let path = "./src/proc/test_data/net/nonexistent_dev";
        let network_data_result = get_network_data_helper(&path);
        assert!(network_data_result.is_err());
    }
}