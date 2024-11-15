use crate::metrics::enhanced::{
    constants::{self, BASE_LAMBDA_INVOCATION_PRICE},
    statfs::statfs_info,
};
use crate::proc::{self, CPUData, NetworkData};
use crate::telemetry::events::ReportMetrics;
use dogstatsd::metric;
use dogstatsd::metric::{Metric, MetricValue};
use dogstatsd::{aggregator::Aggregator, metric::SortedTags};
use std::collections::HashMap;
use std::env::consts::ARCH;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::{
    sync::watch::{Receiver, Sender},
    time::interval,
};
use tracing::debug;
use tracing::error;

pub struct Lambda {
    pub aggregator: Arc<Mutex<Aggregator>>,
    pub config: Arc<crate::config::Config>,
    // Dynamic value tags are the ones we cannot obtain statically from the sandbox
    dynamic_value_tags: HashMap<String, String>,
}

impl Lambda {
    #[must_use]
    pub fn new(aggregator: Arc<Mutex<Aggregator>>, config: Arc<crate::config::Config>) -> Lambda {
        Lambda {
            aggregator,
            config,
            dynamic_value_tags: HashMap::new(),
        }
    }

    /// Set the dynamic value tags that are not available at compile time
    pub fn set_init_tags(&mut self, proactive_initialization: bool, cold_start: bool) {
        self.dynamic_value_tags.remove("cold_start");
        self.dynamic_value_tags.remove("proactive_initialization");

        self.dynamic_value_tags
            .insert(String::from("cold_start"), cold_start.to_string());

        // Only set `proactive_initialization` tag if it is true
        if proactive_initialization {
            self.dynamic_value_tags.insert(
                String::from("proactive_initialization"),
                String::from("true"),
            );
        }
    }

    fn get_dynamic_value_tags(&self) -> Option<SortedTags> {
        let vec_tags: Vec<String> = self
            .dynamic_value_tags
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect();

        let string_tags = vec_tags.join(",");

        SortedTags::parse(&string_tags).ok()
    }

    pub fn increment_invocation_metric(&self) {
        self.increment_metric(constants::INVOCATIONS_METRIC);
    }

    pub fn increment_errors_metric(&self) {
        self.increment_metric(constants::ERRORS_METRIC);
    }

    pub fn increment_timeout_metric(&self) {
        self.increment_metric(constants::TIMEOUTS_METRIC);
    }

    pub fn set_init_duration_metric(&self, init_duration_ms: f64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(
            constants::INIT_DURATION_METRIC.into(),
            MetricValue::distribution(init_duration_ms * constants::MS_TO_SEC),
            self.get_dynamic_value_tags(),
        );

        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert metric: {}", e);
        }
    }

    fn increment_metric(&self, metric_name: &str) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(
            metric_name.into(),
            MetricValue::distribution(1f64),
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert metric: {}", e);
        }
    }

    pub fn set_runtime_duration_metric(&self, duration_ms: f64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(
            constants::RUNTIME_DURATION_METRIC.into(),
            MetricValue::distribution(duration_ms),
            // Datadog expects this value as milliseconds, not seconds
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert runtime duration metric: {}", e);
        }
    }

    pub fn set_post_runtime_duration_metric(&self, duration_ms: f64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = metric::Metric::new(
            constants::POST_RUNTIME_DURATION_METRIC.into(),
            MetricValue::distribution(duration_ms),
            // Datadog expects this value as milliseconds, not seconds
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(metric)
        {
            error!("failed to insert post runtime duration metric: {}", e);
        }
    }

    pub fn generate_network_enhanced_metrics(
        network_data_offset: NetworkData,
        network_data_end: NetworkData,
        aggr: &mut std::sync::MutexGuard<Aggregator>,
        tags: Option<SortedTags>,
    ) {
        let rx_bytes = network_data_end.rx_bytes - network_data_offset.rx_bytes;
        let tx_bytes = network_data_end.tx_bytes - network_data_offset.tx_bytes;
        let total_network = rx_bytes + tx_bytes;

        let metric = Metric::new(
            constants::RX_BYTES_METRIC.into(),
            MetricValue::distribution(rx_bytes),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert rx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TX_BYTES_METRIC.into(),
            MetricValue::distribution(tx_bytes),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert tx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TOTAL_NETWORK_METRIC.into(),
            MetricValue::distribution(total_network),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert total_network metric: {}", e);
        }
    }

    pub fn set_network_enhanced_metrics(&self, network_offset: Option<NetworkData>) {
        if !self.config.enhanced_metrics {
            return;
        }

        if let Some(offset) = network_offset {
            let mut aggr: std::sync::MutexGuard<Aggregator> =
                self.aggregator.lock().expect("lock poisoned");

            match proc::get_network_data() {
                Ok(data) => {
                    Self::generate_network_enhanced_metrics(
                        offset,
                        data,
                        &mut aggr,
                        self.get_dynamic_value_tags(),
                    );
                }
                Err(_e) => {
                    debug!("Could not find data to generate network enhanced metrics");
                }
            }
        } else {
            debug!("Could not find data to generate network enhanced metrics");
        }
    }

    pub(crate) fn generate_cpu_time_enhanced_metrics(
        cpu_data_offset: &CPUData,
        cpu_data_end: &CPUData,
        aggr: &mut std::sync::MutexGuard<Aggregator>,
        tags: Option<SortedTags>,
    ) {
        let cpu_user_time = cpu_data_end.total_user_time_ms - cpu_data_offset.total_user_time_ms;
        let cpu_system_time =
            cpu_data_end.total_system_time_ms - cpu_data_offset.total_system_time_ms;
        let cpu_total_time = cpu_user_time + cpu_system_time;

        let metric = Metric::new(
            constants::CPU_USER_TIME_METRIC.into(),
            MetricValue::distribution(cpu_user_time),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert cpu_user_time metric: {}", e);
        }

        let metric = Metric::new(
            constants::CPU_SYSTEM_TIME_METRIC.into(),
            MetricValue::distribution(cpu_system_time),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert cpu_system_time metric: {}", e);
        }

        let metric = Metric::new(
            constants::CPU_TOTAL_TIME_METRIC.into(),
            MetricValue::distribution(cpu_total_time),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert cpu_total_time metric: {}", e);
        }
    }

    pub fn set_cpu_time_enhanced_metrics(&self, cpu_offset: Option<CPUData>) {
        if !self.config.enhanced_metrics {
            return;
        }

        let mut aggr: std::sync::MutexGuard<Aggregator> =
            self.aggregator.lock().expect("lock poisoned");

        let cpu_data = proc::get_cpu_data();
        match (cpu_offset, cpu_data) {
            (Some(cpu_offset), Ok(cpu_data)) => {
                Self::generate_cpu_time_enhanced_metrics(
                    &cpu_offset,
                    &cpu_data,
                    &mut aggr,
                    self.get_dynamic_value_tags(),
                );
            }
            (_, _) => {
                debug!("Could not find data to generate cpu time enhanced metrics");
            }
        }
    }

    pub(crate) fn generate_cpu_utilization_enhanced_metrics(
        cpu_data_offset: &CPUData,
        cpu_data_end: &CPUData,
        uptime_data_offset: f64,
        uptime_data_end: f64,
        aggr: &mut std::sync::MutexGuard<Aggregator>,
        tags: Option<SortedTags>,
    ) {
        let num_cores = cpu_data_end.individual_cpu_idle_times.len() as f64;
        let uptime = uptime_data_end - uptime_data_offset;
        let total_idle_time = cpu_data_end.total_idle_time_ms - cpu_data_offset.total_idle_time_ms;

        let mut max_idle_time = 0.0;
        let mut min_idle_time = f64::MAX;

        for (cpu_name, cpu_idle_time) in &cpu_data_end.individual_cpu_idle_times {
            if let Some(cpu_idle_time_offset) =
                cpu_data_offset.individual_cpu_idle_times.get(cpu_name)
            {
                let idle_time = cpu_idle_time - cpu_idle_time_offset;
                if idle_time < min_idle_time {
                    min_idle_time = idle_time;
                }
                if idle_time > max_idle_time {
                    max_idle_time = idle_time;
                }
            }
        }

        // Maximally utilized CPU is the one with the least time spent in the idle process
        // Multiply by 100 to report as percentage
        let cpu_max_utilization = ((uptime - min_idle_time) / uptime) * 100.0;

        // Minimally utilized CPU is the one with the most time spent in the idle process
        // Multiply by 100 to report as percentage
        let cpu_min_utilization = ((uptime - max_idle_time) / uptime) * 100.0;

        // CPU total utilization is the proportion of total non-idle time to the total uptime across all cores
        let cpu_total_utilization_decimal =
            ((uptime * num_cores) - total_idle_time) / (uptime * num_cores);
        // Multiply by 100 to report as percentage
        let cpu_total_utilization_pct = cpu_total_utilization_decimal * 100.0;
        // Multiply by num_cores to report in terms of cores
        let cpu_total_utilization = cpu_total_utilization_decimal * num_cores;

        let metric = Metric::new(
            constants::CPU_TOTAL_UTILIZATION_PCT_METRIC.into(),
            MetricValue::distribution(cpu_total_utilization_pct),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert cpu_total_utilization_pct metric: {}", e);
        }

        let metric = Metric::new(
            constants::CPU_TOTAL_UTILIZATION_METRIC.into(),
            MetricValue::distribution(cpu_total_utilization),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert cpu_total_utilization metric: {}", e);
        }

        let metric = Metric::new(
            constants::NUM_CORES_METRIC.into(),
            MetricValue::distribution(num_cores),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert num_cores metric: {}", e);
        }

        let metric = Metric::new(
            constants::CPU_MAX_UTILIZATION_METRIC.into(),
            MetricValue::distribution(cpu_max_utilization),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert cpu_max_utilization metric: {}", e);
        }

        let metric = Metric::new(
            constants::CPU_MIN_UTILIZATION_METRIC.into(),
            MetricValue::distribution(cpu_min_utilization),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert cpu_min_utilization metric: {}", e);
        }
    }

    pub fn set_cpu_utilization_enhanced_metrics(
        &self,
        cpu_offset: Option<CPUData>,
        uptime_offset: Option<f64>,
    ) {
        if !self.config.enhanced_metrics {
            return;
        }

        let mut aggr: std::sync::MutexGuard<Aggregator> =
            self.aggregator.lock().expect("lock poisoned");

        let cpu_data = proc::get_cpu_data();
        let uptime_data = proc::get_uptime();
        match (cpu_offset, cpu_data, uptime_offset, uptime_data) {
            (Some(cpu_offset), Ok(cpu_data), Some(uptime_offset), Ok(uptime_data)) => {
                Self::generate_cpu_utilization_enhanced_metrics(
                    &cpu_offset,
                    &cpu_data,
                    uptime_offset,
                    uptime_data,
                    &mut aggr,
                    self.get_dynamic_value_tags(),
                );
            }
            (_, _, _, _) => {
                debug!("Could not find data to generate cpu utilization enhanced metrics");
            }
        }
    }

    pub fn generate_tmp_enhanced_metrics(
        tmp_max: f64,
        tmp_used: f64,
        aggr: &mut std::sync::MutexGuard<Aggregator>,
        tags: Option<SortedTags>,
    ) {
        let metric = Metric::new(
            constants::TMP_MAX_METRIC.into(),
            MetricValue::distribution(tmp_max),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert tmp_max metric: {}", e);
        }

        let metric = Metric::new(
            constants::TMP_USED_METRIC.into(),
            MetricValue::distribution(tmp_used),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert tmp_used metric: {}", e);
        }

        let tmp_free = tmp_max - tmp_used;
        let metric = Metric::new(
            constants::TMP_FREE_METRIC.into(),
            MetricValue::distribution(tmp_free),
            tags.clone(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert tmp_free metric: {}", e);
        }
    }

    pub fn set_tmp_enhanced_metrics(&self, mut send_metrics: Receiver<()>) {
        if !self.config.enhanced_metrics {
            return;
        }

        let aggr = Arc::clone(&self.aggregator);
        let tags = self.get_dynamic_value_tags();

        tokio::spawn(async move {
            // Set tmp_max and initial value for tmp_used
            let (bsize, blocks, bavail) = match statfs_info(constants::TMP_PATH) {
                Ok(stats) => stats,
                Err(err) => {
                    debug!("Could not emit tmp enhanced metrics. {:?}", err);
                    return;
                }
            };
            let tmp_max = bsize * blocks;
            let mut tmp_used = bsize * (blocks - bavail);

            let mut interval = interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    biased;
                    // When the stop signal is received, generate final metrics
                    _ = send_metrics.changed() => {
                        let mut aggr: std::sync::MutexGuard<Aggregator> =
                            aggr.lock().expect("lock poisoned");
                        Self::generate_tmp_enhanced_metrics(tmp_max, tmp_used, &mut aggr, tags);
                        return;
                    }
                    // Otherwise keep monitoring tmp usage periodically
                    _ = interval.tick() => {
                        let (bsize, blocks, bavail) = match statfs_info(constants::TMP_PATH) {
                            Ok(stats) => stats,
                            Err(err) => {
                                debug!("Could not emit tmp enhanced metrics. {:?}", err);
                                return;
                            }
                        };
                        tmp_used = tmp_used.max(bsize * (blocks - bavail));
                    }
                }
            }
        });
    }

    pub fn generate_fd_enhanced_metrics(
        fd_max: f64,
        fd_use: f64,
        aggr: &mut std::sync::MutexGuard<Aggregator>,
    ) {
        let metric = Metric::new(
            constants::FD_MAX_METRIC.into(),
            MetricValue::distribution(fd_max),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert fd_max metric: {}", e);
        }

        // Check if fd_use value is valid before inserting metric
        if fd_use > 0.0 {
            let metric = Metric::new(
                constants::FD_USE_METRIC.into(),
                MetricValue::distribution(fd_use),
                None,
            );
            if let Err(e) = aggr.insert(metric) {
                error!("Failed to insert fd_use metric: {}", e);
            }
        }
    }

    pub fn generate_threads_enhanced_metrics(
        threads_max: f64,
        threads_use: f64,
        aggr: &mut std::sync::MutexGuard<Aggregator>,
    ) {
        let metric = Metric::new(
            constants::THREADS_MAX_METRIC.into(),
            MetricValue::distribution(threads_max),
            None,
        );
        if let Err(e) = aggr.insert(metric) {
            error!("Failed to insert threads_max metric: {}", e);
        }

        // Check if threads_use value is valid before inserting metric
        if threads_use > 0.0 {
            let metric = Metric::new(
                constants::THREADS_USE_METRIC.into(),
                MetricValue::distribution(threads_use),
                None,
            );
            if let Err(e) = aggr.insert(metric) {
                error!("Failed to insert threads_use metric: {}", e);
            }
        }
    }

    pub fn set_process_enhanced_metrics(&self, mut send_metrics: Receiver<()>) {
        if !self.config.enhanced_metrics {
            return;
        }

        let aggr = Arc::clone(&self.aggregator);

        tokio::spawn(async move {
            // get list of all process ids
            let pids = proc::get_pid_list();

            // Set fd_max and initial value for fd_use to -1
            let fd_max = proc::get_fd_max_data(&pids);
            let mut fd_use = -1_f64;

            // Set threads_max and initial value for threads_use to -1
            let threads_max = proc::get_threads_max_data(&pids);
            let mut threads_use = -1_f64;

            let mut interval = interval(Duration::from_millis(1));
            loop {
                tokio::select! {
                    biased;
                    // When the stop signal is received, generate final metrics
                    _ = send_metrics.changed() => {
                        let mut aggr: std::sync::MutexGuard<Aggregator> =
                            aggr.lock().expect("lock poisoned");
                        Self::generate_fd_enhanced_metrics(fd_max, fd_use, &mut aggr);
                        Self::generate_threads_enhanced_metrics(threads_max, threads_use, &mut aggr);
                        return;
                    }
                    // Otherwise keep monitoring file descriptor and thread usage periodically
                    _ = interval.tick() => {
                        match proc::get_fd_use_data(&pids) {
                            Ok(fd_use_curr) => {
                                fd_use = fd_use.max(fd_use_curr);
                            },
                            Err(_) => {
                                debug!("Could not update file descriptor use enhanced metric.");
                            }
                        };
                        match proc::get_threads_use_data(&pids) {
                            Ok(threads_use_curr) => {
                                threads_use = threads_use.max(threads_use_curr);
                            },
                            Err(_) => {
                                debug!("Could not update threads use enhanced metric.");
                            }
                        };
                    }
                }
            }
        });
    }

    fn calculate_estimated_cost_usd(billed_duration_ms: u64, memory_size_mb: u64) -> f64 {
        let gb_seconds = (billed_duration_ms as f64 * constants::MS_TO_SEC)
            * (memory_size_mb as f64 / constants::MB_TO_GB);

        let price_per_gb = match ARCH {
            "x86_64" => constants::X86_LAMBDA_PRICE_PER_GB_SECOND,
            "aarch64" => constants::ARM_LAMBDA_PRICE_PER_GB_SECOND,
            _ => {
                error!("unsupported architecture: {}", ARCH);
                return 0.0;
            }
        };

        ((BASE_LAMBDA_INVOCATION_PRICE + (gb_seconds * price_per_gb)) * 1e12).round() / 1e12
    }

    pub fn set_report_log_metrics(&self, metrics: &ReportMetrics) {
        if !self.config.enhanced_metrics {
            return;
        }
        let mut aggr: std::sync::MutexGuard<Aggregator> =
            self.aggregator.lock().expect("lock poisoned");
        let metric = metric::Metric::new(
            constants::DURATION_METRIC.into(),
            MetricValue::distribution(metrics.duration_ms * constants::MS_TO_SEC),
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::BILLED_DURATION_METRIC.into(),
            MetricValue::distribution(metrics.billed_duration_ms as f64 * constants::MS_TO_SEC),
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert billed duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MAX_MEMORY_USED_METRIC.into(),
            MetricValue::distribution(metrics.max_memory_used_mb as f64),
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert max memory used metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MEMORY_SIZE_METRIC.into(),
            MetricValue::distribution(metrics.memory_size_mb as f64),
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert memory size metric: {}", e);
        }

        let cost_usd =
            Self::calculate_estimated_cost_usd(metrics.billed_duration_ms, metrics.memory_size_mb);
        let metric = metric::Metric::new(
            constants::ESTIMATED_COST_METRIC.into(),
            MetricValue::distribution(cost_usd),
            self.get_dynamic_value_tags(),
        );
        if let Err(e) = aggr.insert(metric) {
            error!("failed to insert estimated cost metric: {}", e);
        }
    }
}

#[derive(Clone, Debug)]
pub struct EnhancedMetricData {
    pub network_offset: Option<NetworkData>,
    pub cpu_offset: Option<CPUData>,
    pub uptime_offset: Option<f64>,
    pub tmp_chan_tx: Sender<()>,
    pub process_chan_tx: Sender<()>,
}

impl PartialEq for EnhancedMetricData {
    fn eq(&self, other: &Self) -> bool {
        self.network_offset == other.network_offset
            && self.cpu_offset == other.cpu_offset
            && self.uptime_offset == other.uptime_offset
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config;
    use dogstatsd::metric::EMPTY_TAGS;
    const PRECISION: f64 = 0.000_000_01;

    fn setup() -> (Arc<Mutex<Aggregator>>, Arc<config::Config>) {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        (
            Arc::new(Mutex::new(
                Aggregator::new(EMPTY_TAGS, 1024).expect("failed to create aggregator"),
            )),
            config,
        )
    }

    fn assert_sketch(aggregator_mutex: &Mutex<Aggregator>, metric_id: &str, value: f64) {
        let aggregator = aggregator_mutex.lock().unwrap();
        if let Some(e) = aggregator.get_entry_by_id(metric_id.into(), &None) {
            let metric = e.value.get_sketch().unwrap();
            assert!((metric.max().unwrap() - value).abs() < PRECISION);
            assert!((metric.min().unwrap() - value).abs() < PRECISION);
            assert!((metric.sum().unwrap() - value).abs() < PRECISION);
            assert!((metric.avg().unwrap() - value).abs() < PRECISION);
        } else {
            panic!("{}", format!("{metric_id} not found"));
        }
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_increment_invocation_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_invocation_metric();

        assert_sketch(&metrics_aggr, constants::INVOCATIONS_METRIC, 1f64);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_increment_errors_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_errors_metric();

        assert_sketch(&metrics_aggr, constants::ERRORS_METRIC, 1f64);
    }

    #[test]
    fn test_disabled() {
        let (metrics_aggr, no_config) = setup();
        let my_config = Arc::new(config::Config {
            enhanced_metrics: false,
            ..no_config.as_ref().clone()
        });
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_invocation_metric();
        lambda.increment_errors_metric();
        lambda.increment_timeout_metric();
        lambda.set_init_duration_metric(100.0);
        lambda.set_runtime_duration_metric(100.0);
        lambda.set_post_runtime_duration_metric(100.0);
        lambda.set_report_log_metrics(&ReportMetrics {
            duration_ms: 100.0,
            billed_duration_ms: 100,
            max_memory_used_mb: 128,
            memory_size_mb: 256,
            init_duration_ms: Some(50.0),
            restore_duration_ms: None,
        });
        let aggr = metrics_aggr.lock().expect("lock poisoned");
        assert!(aggr
            .get_entry_by_id(constants::INVOCATIONS_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::ERRORS_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::TIMEOUTS_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::INIT_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::RUNTIME_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::POST_RUNTIME_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::BILLED_DURATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::MAX_MEMORY_USED_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::MEMORY_SIZE_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::ESTIMATED_COST_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::RX_BYTES_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::TX_BYTES_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::TOTAL_NETWORK_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::CPU_USER_TIME_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::CPU_SYSTEM_TIME_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::CPU_TOTAL_TIME_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::CPU_TOTAL_UTILIZATION_PCT_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::CPU_TOTAL_UTILIZATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::NUM_CORES_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::CPU_MIN_UTILIZATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::CPU_MAX_UTILIZATION_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::TMP_MAX_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::TMP_USED_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::TMP_FREE_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::FD_MAX_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::FD_USE_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::THREADS_MAX_METRIC.into(), &None)
            .is_none());
        assert!(aggr
            .get_entry_by_id(constants::THREADS_USE_METRIC.into(), &None)
            .is_none());
    }

    #[test]
    fn test_set_report_log_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let report_metrics = ReportMetrics {
            duration_ms: 100.0,
            billed_duration_ms: 100,
            max_memory_used_mb: 128,
            memory_size_mb: 256,
            init_duration_ms: Some(50.0),
            restore_duration_ms: None,
        };
        lambda.set_report_log_metrics(&report_metrics);

        assert_sketch(&metrics_aggr, constants::DURATION_METRIC, 0.1);
        assert_sketch(&metrics_aggr, constants::BILLED_DURATION_METRIC, 0.1);

        assert_sketch(&metrics_aggr, constants::MAX_MEMORY_USED_METRIC, 128.0);
        assert_sketch(&metrics_aggr, constants::MEMORY_SIZE_METRIC, 256.0);
    }

    #[test]
    fn test_set_network_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let network_offset = NetworkData {
            rx_bytes: 180.0,
            tx_bytes: 254.0,
        };
        let network_data = NetworkData {
            rx_bytes: 20180.0,
            tx_bytes: 75000.0,
        };

        Lambda::generate_network_enhanced_metrics(
            network_offset,
            network_data,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
            None,
        );

        assert_sketch(&metrics_aggr, constants::RX_BYTES_METRIC, 20000.0);
        assert_sketch(&metrics_aggr, constants::TX_BYTES_METRIC, 74746.0);
        assert_sketch(&metrics_aggr, constants::TOTAL_NETWORK_METRIC, 94746.0);
    }

    #[test]
    fn test_set_cpu_time_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let mut individual_cpu_idle_time_offsets = HashMap::new();
        individual_cpu_idle_time_offsets.insert("cpu0".to_string(), 10.0);
        individual_cpu_idle_time_offsets.insert("cpu1".to_string(), 20.0);
        let cpu_offset = CPUData {
            total_user_time_ms: 100.0,
            total_system_time_ms: 3.0,
            total_idle_time_ms: 20.0,
            individual_cpu_idle_times: individual_cpu_idle_time_offsets,
        };

        let mut individual_cpu_idle_times_end = HashMap::new();
        individual_cpu_idle_times_end.insert("cpu0".to_string(), 30.0);
        individual_cpu_idle_times_end.insert("cpu1".to_string(), 80.0);
        let cpu_data = CPUData {
            total_user_time_ms: 200.0,
            total_system_time_ms: 56.0,
            total_idle_time_ms: 100.0,
            individual_cpu_idle_times: individual_cpu_idle_times_end,
        };

        Lambda::generate_cpu_time_enhanced_metrics(
            &cpu_offset,
            &cpu_data,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
            None,
        );

        assert_sketch(&metrics_aggr, constants::CPU_USER_TIME_METRIC, 100.0);
        assert_sketch(&metrics_aggr, constants::CPU_SYSTEM_TIME_METRIC, 53.0);
        assert_sketch(&metrics_aggr, constants::CPU_TOTAL_TIME_METRIC, 153.0);
    }

    #[test]
    fn test_set_cpu_utilization_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let mut individual_cpu_idle_time_offsets = HashMap::new();
        individual_cpu_idle_time_offsets.insert("cpu0".to_string(), 10.0);
        individual_cpu_idle_time_offsets.insert("cpu1".to_string(), 30.0);
        let cpu_offset = CPUData {
            total_user_time_ms: 50.0,
            total_system_time_ms: 10.0,
            total_idle_time_ms: 10.0,
            individual_cpu_idle_times: individual_cpu_idle_time_offsets,
        };
        let uptime_offset = 1_891_100.0;

        let mut individual_cpu_idle_times_end = HashMap::new();
        individual_cpu_idle_times_end.insert("cpu0".to_string(), 570.0);
        individual_cpu_idle_times_end.insert("cpu1".to_string(), 600.0);
        let cpu_data = CPUData {
            total_user_time_ms: 200.0,
            total_system_time_ms: 170.0,
            total_idle_time_ms: 1130.0,
            individual_cpu_idle_times: individual_cpu_idle_times_end,
        };
        let uptime_data = 1_891_900.0;

        Lambda::generate_cpu_utilization_enhanced_metrics(
            &cpu_offset,
            &cpu_data,
            uptime_offset,
            uptime_data,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
            None,
        );

        // the differences above and metric values below are from an invocation using the go agent to verify the calculations
        assert_sketch(
            &metrics_aggr,
            constants::CPU_TOTAL_UTILIZATION_PCT_METRIC,
            30.0,
        );
        assert_sketch(&metrics_aggr, constants::CPU_TOTAL_UTILIZATION_METRIC, 0.6);
        assert_sketch(&metrics_aggr, constants::NUM_CORES_METRIC, 2.0);
        assert_sketch(&metrics_aggr, constants::CPU_MAX_UTILIZATION_METRIC, 30.0);
        assert_sketch(&metrics_aggr, constants::CPU_MIN_UTILIZATION_METRIC, 28.75);
    }

    #[test]
    fn test_set_tmp_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let tmp_max = 550461440.0;
        let tmp_used = 12165120.0;

        Lambda::generate_tmp_enhanced_metrics(
            tmp_max,
            tmp_used,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
            None,
        );

        assert_sketch(&metrics_aggr, constants::TMP_MAX_METRIC, 550461440.0);
        assert_sketch(&metrics_aggr, constants::TMP_USED_METRIC, 12165120.0);
        assert_sketch(&metrics_aggr, constants::TMP_FREE_METRIC, 538296320.0);
    }

    #[test]
    fn test_set_fd_enhanced_metrics_valid_fd_use() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let fd_max = 1024.0;
        let fd_use = 175.0;

        Lambda::generate_fd_enhanced_metrics(
            fd_max,
            fd_use,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
        );

        assert_sketch(&metrics_aggr, constants::FD_MAX_METRIC, 1024.0);
        assert_sketch(&metrics_aggr, constants::FD_USE_METRIC, 175.0);
    }

    #[test]
    fn test_set_fd_enhanced_metrics_invalid_fd_use() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let fd_max = 1024.0;
        let fd_use = -1.0;

        Lambda::generate_fd_enhanced_metrics(
            fd_max,
            fd_use,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
        );

        assert_sketch(&metrics_aggr, constants::FD_MAX_METRIC, 1024.0);

        let aggr = lambda.aggregator.lock().expect("lock poisoned");
        assert!(aggr
            .get_entry_by_id(constants::FD_USE_METRIC.into(), &None)
            .is_none());
    }

    #[test]
    fn test_set_threads_enhanced_metrics_valid_threads_use() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let threads_max = 1024.0;
        let threads_use = 40.0;

        Lambda::generate_threads_enhanced_metrics(
            threads_max,
            threads_use,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
        );

        assert_sketch(&metrics_aggr, constants::THREADS_MAX_METRIC, 1024.0);
        assert_sketch(&metrics_aggr, constants::THREADS_USE_METRIC, 40.0);
    }

    #[test]
    fn test_set_threads_enhanced_metrics_invalid_threads_use() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);

        let threads_max = 1024.0;
        let threads_use = -1.0;

        Lambda::generate_threads_enhanced_metrics(
            threads_max,
            threads_use,
            &mut lambda.aggregator.lock().expect("lock poisoned"),
        );

        assert_sketch(&metrics_aggr, constants::THREADS_MAX_METRIC, 1024.0);

        let aggr = lambda.aggregator.lock().expect("lock poisoned");
        assert!(aggr
            .get_entry_by_id(constants::THREADS_USE_METRIC.into(), &None)
            .is_none());
    }
}
