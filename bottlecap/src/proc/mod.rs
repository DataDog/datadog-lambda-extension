pub mod constants;

use std::{
    fs::File,
    io::{self, BufRead},
};

use constants::{LAMDBA_NETWORK_INTERFACE, PROC_NET_DEV_PATH};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NetworkData {
    pub rx_bytes: f64,
    pub tx_bytes: f64,
}

pub fn get_network_data() -> Result<NetworkData, io::Error> {
    get_network_data_from_path(PROC_NET_DEV_PATH)
}

fn get_network_data_from_path(path: &str) -> Result<NetworkData, io::Error> {
    let file = File::open(path)?;
    let reader = io::BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        let mut values = line.split_whitespace();

        if values.next().map_or(false, |interface_name| {
            interface_name.starts_with(LAMDBA_NETWORK_INTERFACE)
        }) {
            // Read the value for received bytes if present
            let rx_bytes: Option<f64> = values.next().and_then(|s| s.parse().ok());

            // Skip over the next 7 values representing metrics for received data and
            // read the value for bytes transmitted if present
            let tx_bytes: Option<f64> = values.nth(7).and_then(|s| s.parse().ok());

            match (rx_bytes, tx_bytes) {
                (Some(rx_val), Some(tx_val)) => {
                    return Ok(NetworkData {
                        rx_bytes: rx_val,
                        tx_bytes: tx_val,
                    })
                }
                (_, _) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "Network data not found",
                    ))
                }
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Network data not found",
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_network_data() {
        let path = "./tests/proc/net/valid_dev";
        let network_data_result = get_network_data_from_path(&path);
        assert!(!network_data_result.is_err());
        let network_data_result = network_data_result.unwrap();
        assert_eq!(network_data_result.rx_bytes, 180.0);
        assert_eq!(network_data_result.tx_bytes, 254.0);

        let path = "./tests/proc/net/invalid_dev_malformed";
        let network_data_result = get_network_data_from_path(&path);
        assert!(network_data_result.is_err());

        let path = "./tests/proc/net/invalid_dev_non_numerical_value";
        let network_data_result = get_network_data_from_path(&path);
        assert!(network_data_result.is_err());

        let path = "./tests/proc/net/missing_interface_dev";
        let network_data_result = get_network_data_from_path(&path);
        assert!(network_data_result.is_err());

        let path = "./tests/proc/net/nonexistent_dev";
        let network_data_result = get_network_data_from_path(&path);
        assert!(network_data_result.is_err());
    }
}
