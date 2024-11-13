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
use tracing::debug;

pub fn get_pid_list() -> Vec<i32> {
    get_pid_list_from_path(PROC_PATH)
}

pub fn get_pid_list_from_path(path: &str) -> Vec<i32> {
    let mut pids = Vec::<i32>::new();

    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => {
            debug!("Could not list /proc files");
            return pids;
        }
    };

    pids.extend(entries.filter_map(|entry| {
        entry.ok().and_then(|dir_entry| {
            // Check if the entry is a directory
            if dir_entry.file_type().ok()?.is_dir() {
                // If the directory name can be parsed as an integer, it will be added to the list
                dir_entry.file_name().to_str()?.parse::<i32>().ok()
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

pub fn get_fd_max_data(pids: &[i32]) -> f64 {
    get_fd_max_data_from_path(PROC_PATH, pids)
}

fn get_fd_max_data_from_path(path: &str, pids: &[i32]) -> f64 {
    let mut fd_max: f64 = 1024.0; // Default to hard limit set by AWS Lambda

    for &pid in pids {
        let limits_path = format!("{}/{}/limits", path, pid);

        let file = match File::open(&limits_path) {
            Ok(file) => file,
            Err(_) => continue,
        };

        let reader = io::BufReader::new(file);
        for line in reader.lines() {
            if let Some(line) = line.ok() {
                if line.starts_with("Max open files") {
                    let values: Vec<&str> = line.split_whitespace().collect();

                    // Line should have 6 items: Max open files [soft limit] [hard limit] [units]
                    if values.len() < 6 {
                        debug!("File descriptor max data not found in file {}", limits_path);
                        break;
                    }

                    // Skip past the first three items ("Max open files") to get the soft limit
                    let fd_max_pid_str = values[3];
                    let fd_max_pid: f64 = match fd_max_pid_str.parse() {
                        Ok(val) => val,
                        Err(_) => {
                            debug!("File descriptor max data not found in file {}", limits_path);
                            break;
                        }
                    };
                    fd_max = fd_max.min(fd_max_pid);
                    break;
                }
            }
        }
    }

    fd_max
}

pub fn get_fd_use_data(pids: &[i32]) -> Result<f64, io::Error> {
    get_fd_use_data_from_path(PROC_PATH, pids)
}

fn get_fd_use_data_from_path(path: &str, pids: &[i32]) -> Result<f64, io::Error> {
    let mut fd_use = 0;

    for pid in pids {
        let fd_path = format!("{}/{}/fd", path, pid);
        let files = match fs::read_dir(fd_path) {
            Ok(files) => files,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "File descriptor use data not found",
                ));
            }
        };
        fd_use += files.count();
    }

    Ok(fd_use as f64)
}

pub fn get_threads_max_data(pids: &[i32]) -> f64 {
    get_threads_max_data_from_path(PROC_PATH, pids)
}

fn get_threads_max_data_from_path(path: &str, pids: &[i32]) -> f64 {
    let mut threads_max: f64 = 1024.0; // Default to hard limit set by AWS Lambda

    for &pid in pids {
        let limits_path = format!("{}/{}/limits", path, pid);

        let file = match File::open(&limits_path) {
            Ok(file) => file,
            Err(_) => continue,
        };

        let reader = io::BufReader::new(file);
        for line in reader.lines() {
            if let Some(line) = line.ok() {
                if line.starts_with("Max processes") {
                    let values: Vec<&str> = line.split_whitespace().collect();

                    // Line should have 5 items: Max processes [soft limit] [hard limit] [units]
                    if values.len() < 5 {
                        debug!("Threads max data not found in file {}", limits_path);
                        break;
                    }

                    // Skip past the first three items ("Max open files") to get the soft limit
                    let fd_max_pid_str = values[2];
                    let fd_max_pid: f64 = match fd_max_pid_str.parse() {
                        Ok(val) => val,
                        Err(_) => {
                            debug!("Threads max data not found in file {}", limits_path);
                            break;
                        }
                    };
                    threads_max = threads_max.min(fd_max_pid);
                    break;
                }
            }
        }
    }

    threads_max
}

pub fn get_threads_use_data(pids: &[i32]) -> Result<f64, io::Error> {
    get_threads_use_data_from_path(PROC_PATH, pids)
}

fn get_threads_use_data_from_path(path: &str, pids: &[i32]) -> Result<f64, io::Error> {
    let mut threads_use = 0;

    for pid in pids {
        let task_path = format!("{}/{}/task", path, pid);
        let files = match fs::read_dir(task_path) {
            Ok(files) => files,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Threads use data not found",
                ));
            }
        };

        for entry in files {
            if let Ok(dir_entry) = entry {
                if let Some(file_type) = dir_entry.file_type().ok() {
                    if file_type.is_dir() {
                        threads_use += 1;
                    }
                }
            }
        }
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
    #[allow(clippy::float_cmp)]
    fn test_get_pid_list() {
        let path = "./tests/proc";
        let mut pids = get_pid_list_from_path(path);
        pids.sort();
        assert_eq!(pids.len(), 2);
        assert_eq!(pids[0], 13);
        assert_eq!(pids[1], 142);

        let path = "./tests/incorrect_folder";
        let pids = get_pid_list_from_path(path);
        assert_eq!(pids.len(), 0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_network_data() {
        let path = "./tests/proc/net/valid_dev";
        let network_data_result = get_network_data_from_path(path_from_root(path).as_str());
        assert!(network_data_result.is_ok());
        let network_data_result = network_data_result.unwrap();
        assert_eq!(network_data_result.rx_bytes, 180.0);
        assert_eq!(network_data_result.tx_bytes, 254.0);

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
    #[allow(clippy::float_cmp)]
    fn test_get_cpu_data() {
        let path = "./tests/proc/stat/valid_stat";
        let cpu_data_result = get_cpu_data_from_path(path_from_root(path).as_str());
        assert!(cpu_data_result.is_ok());
        let cpu_data = cpu_data_result.unwrap();
        assert_eq!(cpu_data.total_user_time_ms, 23370.0);
        assert_eq!(cpu_data.total_system_time_ms, 1880.0);
        assert_eq!(cpu_data.total_idle_time_ms, 178_380.0);
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
    #[allow(clippy::float_cmp)]
    fn test_get_uptime_data() {
        let path = "./tests/proc/uptime/valid_uptime";
        let uptime_data_result = get_uptime_from_path(path_from_root(path).as_str());
        assert!(uptime_data_result.is_ok());
        let uptime_data = uptime_data_result.unwrap();
        assert_eq!(uptime_data, 3_213_103_123_000.0);

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
    #[allow(clippy::float_cmp)]
    fn test_get_fd_max_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path);
        let fd_max = get_fd_max_data_from_path(path, &pids);
        assert_eq!(fd_max, 900.0);

        let path = "./tests/proc/process/invalid_malformed";
        let fd_max = get_fd_max_data_from_path(path, &pids);
        assert_eq!(fd_max, 1024.0); // assert that fd_max is equal to AWS Lambda limit

        let path = "./tests/proc/process/invalid_missing";
        let fd_max = get_fd_max_data_from_path(path, &pids);
        assert_eq!(fd_max, 1024.0); // assert that fd_max is equal to AWS Lambda limit
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_fd_use_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path);
        let fd_use_result = get_fd_use_data_from_path(path, &pids);
        assert!(fd_use_result.is_ok());
        let fd_use = fd_use_result.unwrap();
        assert_eq!(fd_use, 5.0);

        let path = "./tests/proc/process/invalid_missing";
        let fd_use_result = get_fd_use_data_from_path(path, &pids);
        assert!(fd_use_result.is_err());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_threads_max_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path);
        let fd_max = get_threads_max_data_from_path(path, &pids);
        assert_eq!(fd_max, 1024.0);

        let path = "./tests/proc/process/invalid_malformed";
        let fd_max = get_threads_max_data_from_path(path, &pids);
        assert_eq!(fd_max, 1024.0); // assert that threads_max is equal to AWS Lambda limit

        let path = "./tests/proc/process/invalid_missing";
        let fd_max = get_threads_max_data_from_path(path, &pids);
        assert_eq!(fd_max, 1024.0); // assert that threads_max is equal to AWS Lambda limit
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_get_threads_use_data() {
        let path = "./tests/proc/process/valid";
        let pids = get_pid_list_from_path(path);
        let threads_use_result = get_threads_use_data_from_path(path, &pids);
        assert!(threads_use_result.is_ok());
        let threads_use = threads_use_result.unwrap();
        assert_eq!(threads_use, 5.0);

        let path = "./tests/proc/process/invalid_missing";
        let threads_use_result = get_threads_use_data_from_path(path, &pids);
        assert!(threads_use_result.is_err());
    }
}
