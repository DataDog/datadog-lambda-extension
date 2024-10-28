pub mod clock;
pub mod constants;

use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufRead},
};

use constants::{
    LAMDBA_NETWORK_INTERFACE, PROC_NET_DEV_PATH, PROC_STAT_PATH, PROC_UPTIME_PATH,
};

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

#[derive(Clone, Debug, PartialEq)]
pub struct CPUData {
    pub total_user_time_ms: f64,
    pub total_system_time_ms: f64,
    pub total_idle_time_ms: f64,
    pub individual_cpu_idle_times: HashMap<String, f64>,
}

pub fn get_cpu_data() -> Result<CPUData, io::Error> {
    get_cpu_data_from_path(PROC_STAT_PATH)
}

fn get_cpu_data_from_path(path: &str) -> Result<CPUData, io::Error> {
    let file = File::open(path)?;
    let reader = io::BufReader::new(file);

    let mut cpu_data = CPUData {
        total_user_time_ms: 0.0,
        total_system_time_ms: 0.0,
        total_idle_time_ms: 0.0,
        individual_cpu_idle_times: HashMap::new(),
    };

    // SC_CLK_TCK is the system clock frequency in ticks per second
    // We'll use this to convert CPU times from user HZ to milliseconds
    let clktck = clock::get_clk_tck()? as f64;

    for line in reader.lines() {
        let line = line?;
        let mut values = line.split_whitespace();

        if let Some(label) = values.next() {
            if label == "cpu" {
                // Parse CPU times for total user, system, and idle
                let user: Option<f64> = values.next().and_then(|s| s.parse().ok());
                values.next(); // skip "nice"
                let system: Option<f64> = values.next().and_then(|s| s.parse().ok());
                let idle: Option<f64> = values.next().and_then(|s| s.parse().ok());

                match (user, system, idle) {
                    (Some(user_val), Some(system_val), Some(idle_val)) => {
                        cpu_data.total_user_time_ms = (1000.0 * user_val) / clktck;
                        cpu_data.total_system_time_ms = (1000.0 * system_val) / clktck;
                        cpu_data.total_idle_time_ms = (1000.0 * idle_val) / clktck;
                    }
                    (_, _, _) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Failed to parse CPU data",
                        ))
                    }
                }
            } else if label.starts_with("cpu") { // i.e. "cpu0", "cpu1"
                // Parse per-core idle times
                // Skip the first three values (user, nice, system) and get the 4th value (idle)
                let idle: Option<f64> = values.nth(3).and_then(|s| s.parse().ok());

                match idle {
                    Some(idle_val) => {
                        cpu_data
                            .individual_cpu_idle_times
                            .insert(label.to_string(), (1000.0 * idle_val) / clktck);
                    }
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Failed to parse per-core CPU data",
                        ))
                    }
                }
            }
        }
    }

    if cpu_data.individual_cpu_idle_times.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Per-core CPU data not found",
        ));
    }

    Ok(cpu_data)
}

pub fn get_uptime() -> Result<f64, io::Error> {
    get_uptime_from_path(PROC_UPTIME_PATH)
}

fn get_uptime_from_path(path: &str) -> Result<f64, io::Error> {
    let file = File::open(path)?;
    let reader = io::BufReader::new(file);

    if let Some(Ok(line)) = reader.lines().next() {
        let mut values = line.split_whitespace();

        let uptime: Option<f64> = values.next().and_then(|s| s.parse().ok());
        let idle: Option<f64> = values.next().and_then(|s| s.parse().ok());

        match (uptime, idle) {
            // Check that the file is correctly formatted (i.e. has both values)
            (Some(uptime_val), Some(_idle_val)) => return Ok(uptime_val * 1000.0),
            (_, _) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Failed to parse uptime data",
                ));
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Uptime data not found",
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

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_cpu_data() {
        let path = "./tests/proc/stat/valid_stat";
        let cpu_data_result = get_cpu_data_from_path(&path);
        assert!(!cpu_data_result.is_err());
        let cpu_data = cpu_data_result.unwrap();
        assert_eq!(cpu_data.total_user_time_ms, 23370.0);
        assert_eq!(cpu_data.total_system_time_ms, 1880.0);
        assert_eq!(cpu_data.total_idle_time_ms, 178380.0);
        assert_eq!(cpu_data.individual_cpu_idle_times.len(), 2);
        assert_eq!(
            *cpu_data
                .individual_cpu_idle_times
                .get("cpu0")
                .expect("cpu0 not found"),
            91880.0
        );
        assert_eq!(
            *cpu_data
                .individual_cpu_idle_times
                .get("cpu1")
                .expect("cpu1 not found"),
            86490.0
        );

        let path = "./tests/proc/stat/invalid_stat_non_numerical_value_1";
        let cpu_data_result = get_cpu_data_from_path(&path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_non_numerical_value_2";
        let cpu_data_result = get_cpu_data_from_path(&path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_malformed_first_line";
        let cpu_data_result = get_cpu_data_from_path(&path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_malformed_per_cpu_line";
        let cpu_data_result = get_cpu_data_from_path(&path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_missing_cpun_data";
        let cpu_data_result = get_cpu_data_from_path(&path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/nonexistent_stat";
        let cpu_data_result = get_cpu_data_from_path(&path);
        assert!(cpu_data_result.is_err());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_uptime_data() {
        let path = "./tests/proc/uptime/valid_uptime";
        let uptime_data_result = get_uptime_from_path(&path);
        assert!(!uptime_data_result.is_err());
        let uptime_data = uptime_data_result.unwrap();
        assert_eq!(uptime_data, 3213103123000.0);

        let path = "./tests/proc/uptime/invalid_data_uptime";
        let uptime_data_result = get_uptime_from_path(&path);
        assert!(uptime_data_result.is_err());

        let path = "./tests/proc/uptime/malformed_uptime";
        let uptime_data_result = get_uptime_from_path(&path);
        assert!(uptime_data_result.is_err());

        let path = "./tests/proc/uptime/nonexistent_uptime";
        let uptime_data_result = get_uptime_from_path(&path);
        assert!(uptime_data_result.is_err());
    }
}
