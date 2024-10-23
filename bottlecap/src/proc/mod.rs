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

        // Check for the line containing lambda network data by interface name
        if let Some(interface_name) = values.next() {
            if interface_name.starts_with(LAMDBA_NETWORK_INTERFACE) {
                // Read the value for bytes received if present, otherwise break and return error
                let rx_bytes: f64 = match values.next().and_then(|s| s.parse().ok()) {
                    Some(value) => value,
                    None => break,
                };
                // Read the value for bytes transmitted if present, otherwise break and return error
                let tx_bytes: f64 = match values.nth(7).and_then(|s| s.parse().ok()) {
                    Some(value) => value,
                    None => break,
                };

                return Ok(NetworkData { rx_bytes, tx_bytes });
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
