pub mod clock;
pub mod constants;

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufRead},
};

use constants::{
    LAMDBA_NETWORK_INTERFACE, PROC_NET_DEV_PATH, PROC_PATH, PROC_STAT_PATH, PROC_UPTIME_PATH,
};
use regex::Regex;
use tracing::debug;
use std::time::Instant;

#[must_use]
pub fn get_pid_list() -> Vec<i64> {
    get_pid_list_from_path(PROC_PATH)
}

pub fn get_pid_list_from_path(path: &str) -> Vec<i64> {
    let mut pids = Vec::<i64>::new();

    let Ok(entries) = fs::read_dir(path) else {
        debug!("Could not list /proc files");
        return pids;
    };

    pids.extend(entries.filter_map(|entry| {
        entry.ok().and_then(|dir_entry| {
            // Check if the entry is a directory
            if dir_entry.file_type().ok()?.is_dir() {
                // If the directory name can be parsed as an integer, it will be added to the list
                dir_entry.file_name().to_str()?.parse::<i64>().ok()
            } else {
                None
            }
        })
    }));

    pids
}

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
                        // Divide values by clock tick to covert to seconds, then multiply by 1000 to convert to ms
                        cpu_data.total_user_time_ms = (user_val / clktck) * 1000.0;
                        cpu_data.total_system_time_ms = (system_val / clktck) * 1000.0;
                        cpu_data.total_idle_time_ms = (idle_val / clktck) * 1000.0;
                    }
                    (_, _, _) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Failed to parse CPU data",
                        ))
                    }
                }
            } else if label.starts_with("cpu") {
                // Parse per core (i.e. "cpu0", "cpu1", etc.) idle times
                // Skip the first three values (user, nice, system) and get the 4th value (idle)
                let idle: Option<f64> = values.nth(3).and_then(|s| s.parse().ok());

                match idle {
                    Some(idle_val) => {
                        // Divide value by clock tick to covert to seconds, then multiply by 1000 to convert to ms
                        cpu_data
                            .individual_cpu_idle_times
                            .insert(label.to_string(), (idle_val / clktck) * 1000.0);
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

    if let Some(line) = reader.lines().next() {
        let line = line?;
        let mut values = line.split_whitespace();

        let uptime: Option<f64> = values.next().and_then(|s| s.parse().ok());
        let idle: Option<f64> = values.next().and_then(|s| s.parse().ok());

        match (uptime, idle) {
            // Check that the file is correctly formatted (i.e. has both values)
            // Multiply val by 1000 to convert seconds to milliseconds
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

#[must_use]
pub fn get_fd_max_data(pids: &[i64]) -> f64 {
    get_fd_max_data_from_path(PROC_PATH, pids)
}

fn get_fd_max_data_from_path(path: &str, pids: &[i64]) -> f64 {
    let mut fd_max = constants::LAMBDA_FILE_DESCRIPTORS_DEFAULT_LIMIT;
    // regex to capture the soft limit value (first numeric value after the title)
    let re = Regex::new(r"^Max open files\s+(\d+)").expect("Failed to create regex");

    for &pid in pids {
        let limits_path = format!("{path}/{pid}/limits");
        let Ok(file) = File::open(&limits_path) else {
            continue;
        };

        let reader = io::BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(line_items) = re.captures(&line) {
                if let Ok(fd_max_pid) = line_items[1].parse() {
                    fd_max = fd_max.min(fd_max_pid);
                } else {
                    debug!("File descriptor max data not found in file {}", limits_path);
                }
                break;
            }
        }
    }

    fd_max
}

pub fn get_fd_use_data(pids: &[i64]) -> Result<f64, io::Error> {
    get_fd_use_data_from_path(PROC_PATH, pids)
}

fn get_fd_use_data_from_path(path: &str, pids: &[i64]) -> Result<f64, io::Error> {
    let mut fd_use = 0;

    for &pid in pids {
        let fd_path = format!("{path}/{pid}/fd");
        let Ok(files) = fs::read_dir(fd_path) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "File descriptor use data not found",
            ));
        };
        let count = files.count();
        fd_use += count;
    }

    Ok(fd_use as f64)
}

#[must_use]
pub fn get_threads_max_data(pids: &[i64]) -> f64 {
    get_threads_max_data_from_path(PROC_PATH, pids)
}

fn get_threads_max_data_from_path(path: &str, pids: &[i64]) -> f64 {
    let mut threads_max = constants::LAMBDA_EXECUTION_PROCESSES_DEFAULT_LIMIT;
    // regex to capture the soft limit value (first numeric value after the title)
    let re = Regex::new(r"^Max processes\s+(\d+)").expect("Failed to create regex");

    for &pid in pids {
        let limits_path = format!("{path}/{pid}/limits");
        let Ok(file) = File::open(&limits_path) else {
            continue;
        };

        let reader = io::BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(line_items) = re.captures(&line) {
                if let Ok(threads_max_pid) = line_items[1].parse() {
                    threads_max = threads_max.min(threads_max_pid);
                } else {
                    debug!("Threads max data not found in file {}", limits_path);
                }
                break;
            }
        }
    }

    threads_max
}

pub fn get_threads_use_data(pids: &[i64]) -> Result<f64, io::Error> {
    get_threads_use_data_from_path(PROC_PATH, pids)
}

fn get_threads_use_data_from_path(path: &str, pids: &[i64]) -> Result<f64, io::Error> {
    let mut threads_use = 0;

    for &pid in pids {
        let task_path = format!("{path}/{pid}/task");
        let Ok(files) = fs::read_dir(task_path) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Threads use data not found",
            ));
        };

        threads_use += files
            .flatten()
            .filter_map(|dir_entry| dir_entry.file_type().ok())
            .filter(fs::FileType::is_dir)
            .count();
    }

    Ok(threads_use as f64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path_from_root(file: &str) -> String {
        let mut safe_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        safe_path.push(file);
        safe_path.to_str().unwrap().to_string()
    }

    #[test]
    fn test_get_pid_list() {
        let path = "./tests/proc";
        let mut pids = get_pid_list_from_path(path_from_root(path).as_str());
        pids.sort_unstable();
        assert_eq!(pids.len(), 2);
        assert_eq!(pids[0], 13);
        assert_eq!(pids[1], 142);

        let path = "./tests/incorrect_folder";
        let pids = get_pid_list_from_path(path);
        assert_eq!(pids.len(), 0);
    }

    #[test]
    fn test_get_network_data() {
        let path = "./tests/proc/net/valid_dev";
        let network_data_result = get_network_data_from_path(path_from_root(path).as_str());
        assert!(network_data_result.is_ok());
        let network_data = network_data_result.unwrap();
        assert!((network_data.rx_bytes - 180.0).abs() < f64::EPSILON);
        assert!((network_data.tx_bytes - 254.0).abs() < f64::EPSILON);

        let path = "./tests/proc/net/invalid_dev_malformed";
        let network_data_result = get_network_data_from_path(path);
        assert!(network_data_result.is_err());

        let path = "./tests/proc/net/invalid_dev_non_numerical_value";
        let network_data_result = get_network_data_from_path(path);
        assert!(network_data_result.is_err());

        let path = "./tests/proc/net/missing_interface_dev";
        let network_data_result = get_network_data_from_path(path);
        assert!(network_data_result.is_err());

        let path = "./tests/proc/net/nonexistent_dev";
        let network_data_result = get_network_data_from_path(path);
        assert!(network_data_result.is_err());
    }

    #[test]
    fn test_get_cpu_data() {
        let path = "./tests/proc/stat/valid_stat";
        let cpu_data_result = get_cpu_data_from_path(path_from_root(path).as_str());
        assert!(cpu_data_result.is_ok());
        let cpu_data = cpu_data_result.unwrap();
        assert!((cpu_data.total_user_time_ms - 23370.0).abs() < f64::EPSILON);
        assert!((cpu_data.total_system_time_ms - 1880.0).abs() < f64::EPSILON);
        assert!((cpu_data.total_idle_time_ms - 178_380.0).abs() < f64::EPSILON);
        assert_eq!(cpu_data.individual_cpu_idle_times.len(), 2);
        assert!(
            (*cpu_data
                .individual_cpu_idle_times
                .get("cpu0")
                .expect("cpu0 not found")
                - 91880.0)
                .abs()
                < f64::EPSILON
        );
        assert!(
            (*cpu_data
                .individual_cpu_idle_times
                .get("cpu1")
                .expect("cpu1 not found")
                - 86490.0)
                .abs()
                < f64::EPSILON
        );

        let path = "./tests/proc/stat/invalid_stat_non_numerical_value_1";
        let cpu_data_result = get_cpu_data_from_path(path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_non_numerical_value_2";
        let cpu_data_result = get_cpu_data_from_path(path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_malformed_first_line";
        let cpu_data_result = get_cpu_data_from_path(path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_malformed_per_cpu_line";
        let cpu_data_result = get_cpu_data_from_path(path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/invalid_stat_missing_cpun_data";
        let cpu_data_result = get_cpu_data_from_path(path);
        assert!(cpu_data_result.is_err());

        let path = "./tests/proc/stat/nonexistent_stat";
        let cpu_data_result = get_cpu_data_from_path(path);
        assert!(cpu_data_result.is_err());
    }

    #[test]
    fn test_get_uptime_data() {
        let path = "./tests/proc/uptime/valid_uptime";
        let uptime_data_result = get_uptime_from_path(path_from_root(path).as_str());
        assert!(uptime_data_result.is_ok());
        let uptime_data = uptime_data_result.unwrap();
        assert!((uptime_data - 3_213_103_123_000.0).abs() < f64::EPSILON);

        let path = "./tests/proc/uptime/invalid_data_uptime";
        let uptime_data_result = get_uptime_from_path(path);
        assert!(uptime_data_result.is_err());

        let path = "./tests/proc/uptime/malformed_uptime";
        let uptime_data_result = get_uptime_from_path(path);
        assert!(uptime_data_result.is_err());

        let path = "./tests/proc/uptime/nonexistent_uptime";
        let uptime_data_result = get_uptime_from_path(path);
        assert!(uptime_data_result.is_err());
    }

    #[test]
    fn test_get_fd_max_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path_from_root(path).as_str());
        let fd_max = get_fd_max_data_from_path(path, &pids);
        assert!((fd_max - 900.0).abs() < f64::EPSILON);

        let path = "./tests/proc/process/invalid_malformed";
        let fd_max = get_fd_max_data_from_path(path, &pids);
        // assert that fd_max is equal to AWS Lambda limit
        assert!((fd_max - constants::LAMBDA_FILE_DESCRIPTORS_DEFAULT_LIMIT).abs() < f64::EPSILON);

        let path = "./tests/proc/process/invalid_missing";
        let fd_max = get_fd_max_data_from_path(path, &pids);
        // assert that fd_max is equal to AWS Lambda limit
        assert!((fd_max - constants::LAMBDA_FILE_DESCRIPTORS_DEFAULT_LIMIT).abs() < f64::EPSILON);
    }

    #[test]
    fn test_get_fd_use_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path_from_root(path).as_str());
        let fd_use_result = get_fd_use_data_from_path(path, &pids);
        assert!(fd_use_result.is_ok());
        let fd_use = fd_use_result.unwrap();
        assert!((fd_use - 5.0).abs() < f64::EPSILON);

        let path = "./tests/proc/process/invalid_missing";
        let fd_use_result = get_fd_use_data_from_path(path, &pids);
        assert!(fd_use_result.is_err());
    }

    #[test]
    fn test_get_threads_max_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path_from_root(path).as_str());
        let threads_max = get_threads_max_data_from_path(path, &pids);
        assert!((threads_max - 1024.0).abs() < f64::EPSILON);

        let path = "./tests/proc/process/invalid_malformed";
        let threads_max = get_threads_max_data_from_path(path, &pids);
        // assert that threads_max is equal to AWS Lambda limit
        assert!(
            (threads_max - constants::LAMBDA_EXECUTION_PROCESSES_DEFAULT_LIMIT).abs()
                < f64::EPSILON
        );

        let path = "./tests/proc/process/invalid_missing";
        let threads_max = get_threads_max_data_from_path(path, &pids);
        // assert that threads_max is equal to AWS Lambda limit
        assert!(
            (threads_max - constants::LAMBDA_EXECUTION_PROCESSES_DEFAULT_LIMIT).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_get_threads_use_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path_from_root(path).as_str());
        let threads_use_result = get_threads_use_data_from_path(path, &pids);
        assert!(threads_use_result.is_ok());
        let threads_use = threads_use_result.unwrap();
        assert!((threads_use - 5.0).abs() < f64::EPSILON);

        let path = "./tests/proc/process/invalid_missing";
        let threads_use_result = get_threads_use_data_from_path(path, &pids);
        assert!(threads_use_result.is_err());
    }
}
