use crate::extension::telemetry::events::{InitType, ReportMetrics, RuntimeDoneMetrics};
use crate::metrics::enhanced::{
    constants::{self, BASE_LAMBDA_INVOCATION_PRICE},
    statfs::statfs_info,
};
use crate::proc::{self, CPUData, NetworkData};
use dogstatsd::metric::SortedTags;
use dogstatsd::metric::{Metric, MetricValue};
use dogstatsd::{aggregator_service::AggregatorHandle, metric};
use std::collections::HashMap;
use std::env::consts::ARCH;
use std::sync::Arc;
use std::time::Duration;
use tokio::{
    sync::watch::{Receiver, Sender},
    time::interval,
};
use tracing::debug;
use tracing::error;

pub struct Lambda {
    pub aggr_handle: AggregatorHandle,
    pub config: Arc<crate::config::Config>,
    // Dynamic value tags are the ones we cannot obtain statically from the sandbox
    dynamic_value_tags: HashMap<String, String>,
    invoked_received: bool,
}

impl Lambda {
    #[must_use]
    pub fn new(aggregator: AggregatorHandle, config: Arc<crate::config::Config>) -> Lambda {
        Lambda {
            aggr_handle: aggregator,
            config,
            dynamic_value_tags: HashMap::new(),
            invoked_received: false,
        }
    }

    /// Set the init tags in `dynamic_value_tags`
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

    /// Sets the runtime tag in `dynamic_value_tags`
    pub fn set_runtime_tag(&mut self, runtime: &str) {
        self.dynamic_value_tags
            .insert(String::from("runtime"), runtime.to_string());
    }

    fn tags_to_sorted_tags(tags: &HashMap<String, String>) -> Option<SortedTags> {
        let vec_tags: Vec<String> = tags.iter().map(|(k, v)| format!("{k}:{v}")).collect();

        let string_tags = vec_tags.join(",");

        SortedTags::parse(&string_tags).ok()
    }

    fn get_dynamic_value_tags(&self) -> Option<SortedTags> {
        Self::tags_to_sorted_tags(&self.dynamic_value_tags)
    }

    fn get_combined_tags(&self, additional_tags: &HashMap<String, String>) -> Option<SortedTags> {
        if additional_tags.is_empty() {
            return self.get_dynamic_value_tags();
        }

        let mut combined_tags = self.dynamic_value_tags.clone();
        combined_tags.extend(additional_tags.clone());

        Self::tags_to_sorted_tags(&combined_tags)
    }

    pub fn increment_invocation_metric(&self, timestamp: i64) {
        self.increment_metric(constants::INVOCATIONS_METRIC, timestamp);
    }

    pub fn increment_errors_metric(&self, timestamp: i64) {
        self.increment_metric(constants::ERRORS_METRIC, timestamp);
    }

    pub fn increment_timeout_metric(&self, timestamp: i64) {
        self.increment_metric(constants::TIMEOUTS_METRIC, timestamp);
    }

    // This function is called in three cases:
    // 1. Runtime-specific OOM error (can happen in .NET, Node.js and Java as far as we know)
    // 2. PlatformRuntimeDone event reports "error_type: Runtime.OutOfMemory" (can happen in Ruby and Python as far as we know)
    // 3. PlatformReport event reports "max_memory_used_mb == memory_size_mb" (can happen in many runtimes, but
    //    we only call increment_oom_metric() for provided.al runtimes)
    // This is our best effort to cover different cases without double counting. We can adjust this if we find more cases.
    pub fn increment_oom_metric(&self, timestamp: i64) {
        self.increment_metric(constants::OUT_OF_MEMORY_METRIC, timestamp);
    }

    /// Set up a metric tracking configuration load issue with details
    pub fn set_config_load_issue_metric(&self, timestamp: i64, reason_msg: &str) {
        let dynamic_tags = self.get_combined_tags(&HashMap::from([(
            "reason".to_string(),
            reason_msg.to_string(),
        )]));
        self.increment_metric_with_tags(
            constants::DATADOG_SERVERLESS_EXTENSION_FAILOVER_CONFIG_ISSUE_METRIC,
            timestamp,
            dynamic_tags,
        );
    }

    pub fn set_init_duration_metric(
        &mut self,
        init_type: InitType,
        init_duration_ms: f64,
        timestamp: i64,
    ) {
        if !self.config.enhanced_metrics {
            return;
        }
        self.dynamic_value_tags
            .insert(String::from("init_type"), init_type.to_string());
        let metric = Metric::new(
            constants::INIT_DURATION_METRIC.into(),
            MetricValue::distribution(init_duration_ms * constants::MS_TO_SEC),
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );

        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert metric: {}", e);
        }
    }

    pub fn set_invoked_received(&mut self) {
        self.invoked_received = true;
    }

    fn increment_metric(&self, metric_name: &str, timestamp: i64) {
        self.increment_metric_with_tags(metric_name, timestamp, self.get_dynamic_value_tags());
    }

    /// Helper function to emit metric with supplied tags
    fn increment_metric_with_tags(
        &self,
        metric_name: &str,
        timestamp: i64,
        tags: Option<SortedTags>,
    ) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(
            metric_name.into(),
            MetricValue::distribution(1f64),
            tags,
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert metric: {}", e);
        }
    }

    pub fn set_runtime_done_metrics(&self, metrics: &RuntimeDoneMetrics, timestamp: i64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = Metric::new(
            constants::RUNTIME_DURATION_METRIC.into(),
            MetricValue::distribution(metrics.duration_ms),
            // Datadog expects this value as milliseconds, not seconds
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert runtime duration metric: {}", e);
        }

        if let Some(produced_bytes) = metrics.produced_bytes {
            let metric = Metric::new(
                constants::PRODUCED_BYTES_METRIC.into(),
                MetricValue::distribution(produced_bytes as f64),
                // Datadog expects this value as milliseconds, not seconds
                self.get_dynamic_value_tags(),
                Some(timestamp),
            );
            if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
                error!("failed to insert produced bytes metric: {}", e);
            }
        }
    }

    pub fn set_shutdown_metric(&self, timestamp: i64) {
        if !self.config.enhanced_metrics {
            return;
        }
        self.increment_metric(constants::SHUTDOWNS_METRIC, timestamp);
    }

    pub fn set_unused_init_metric(&self, timestamp: i64) {
        if !self.invoked_received {
            self.increment_metric(constants::UNUSED_INIT, timestamp);
        }
    }

    pub fn set_post_runtime_duration_metric(&self, duration_ms: f64, timestamp: i64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = metric::Metric::new(
            constants::POST_RUNTIME_DURATION_METRIC.into(),
            MetricValue::distribution(duration_ms),
            // Datadog expects this value as milliseconds, not seconds
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert post runtime duration metric: {}", e);
        }
    }

    pub fn generate_network_enhanced_metrics(
        network_data_offset: NetworkData,
        network_data_end: NetworkData,
        aggr: &AggregatorHandle,
        tags: Option<SortedTags>,
    ) {
        let now = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let rx_bytes = network_data_end.rx_bytes - network_data_offset.rx_bytes;
        let tx_bytes = network_data_end.tx_bytes - network_data_offset.tx_bytes;
        let total_network = rx_bytes + tx_bytes;

        let metric = Metric::new(
            constants::RX_BYTES_METRIC.into(),
            MetricValue::distribution(rx_bytes),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert rx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TX_BYTES_METRIC.into(),
            MetricValue::distribution(tx_bytes),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert tx_bytes metric: {}", e);
        }

        let metric = Metric::new(
            constants::TOTAL_NETWORK_METRIC.into(),
            MetricValue::distribution(total_network),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert total_network metric: {}", e);
        }
    }

    pub fn set_network_enhanced_metrics(&self, network_offset: Option<NetworkData>) {
        if !self.config.enhanced_metrics {
            return;
        }

        if let Some(offset) = network_offset {
            let aggr_handle = self.aggr_handle.clone();

            match proc::get_network_data() {
                Ok(data) => {
                    Self::generate_network_enhanced_metrics(
                        offset,
                        data,
                        &aggr_handle,
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
        aggr: &AggregatorHandle,
        tags: Option<SortedTags>,
    ) {
        let cpu_user_time = cpu_data_end.total_user_time_ms - cpu_data_offset.total_user_time_ms;
        let cpu_system_time =
            cpu_data_end.total_system_time_ms - cpu_data_offset.total_system_time_ms;
        let cpu_total_time = cpu_user_time + cpu_system_time;
        let now = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let metric = Metric::new(
            constants::CPU_USER_TIME_METRIC.into(),
            MetricValue::distribution(cpu_user_time),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert cpu_user_time metric: {}", e);
        }

        let metric = Metric::new(
            constants::CPU_SYSTEM_TIME_METRIC.into(),
            MetricValue::distribution(cpu_system_time),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert cpu_system_time metric: {}", e);
        }

        let metric = Metric::new(
            constants::CPU_TOTAL_TIME_METRIC.into(),
            MetricValue::distribution(cpu_total_time),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert cpu_total_time metric: {}", e);
        }
    }

    pub fn set_cpu_time_enhanced_metrics(&self, cpu_offset: Option<CPUData>) {
        if !self.config.enhanced_metrics {
            return;
        }

        let aggr_handle = self.aggr_handle.clone();

        let cpu_data = proc::get_cpu_data();
        match (cpu_offset, cpu_data) {
            (Some(cpu_offset), Ok(cpu_data)) => {
                Self::generate_cpu_time_enhanced_metrics(
                    &cpu_offset,
                    &cpu_data,
                    &aggr_handle,
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
        aggr: &AggregatorHandle,
        tags: Option<SortedTags>,
    ) {
        let num_cores = cpu_data_end.individual_cpu_idle_times.len() as f64;
        let uptime = uptime_data_end - uptime_data_offset;
        let total_idle_time = cpu_data_end.total_idle_time_ms - cpu_data_offset.total_idle_time_ms;

        let mut max_idle_time = 0.0;
        let mut min_idle_time = f64::MAX;
        let now = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
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

        let metrics = vec![
            Metric::new(
                constants::CPU_TOTAL_UTILIZATION_PCT_METRIC.into(),
                MetricValue::distribution(cpu_total_utilization_pct),
                tags.clone(),
                Some(now),
            ),
            Metric::new(
                constants::CPU_TOTAL_UTILIZATION_METRIC.into(),
                MetricValue::distribution(cpu_total_utilization),
                tags.clone(),
                Some(now),
            ),
            Metric::new(
                constants::NUM_CORES_METRIC.into(),
                MetricValue::distribution(num_cores),
                tags.clone(),
                Some(now),
            ),
            Metric::new(
                constants::CPU_MAX_UTILIZATION_METRIC.into(),
                MetricValue::distribution(cpu_max_utilization),
                tags.clone(),
                Some(now),
            ),
            Metric::new(
                constants::CPU_MIN_UTILIZATION_METRIC.into(),
                MetricValue::distribution(cpu_min_utilization),
                tags,
                Some(now),
            ),
        ];

        if let Err(e) = aggr.insert_batch(metrics) {
            error!("Failed to insert cpu utilization metrics: {}", e);
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

        let aggr_handle = self.aggr_handle.clone();

        let cpu_data = proc::get_cpu_data();
        let uptime_data = proc::get_uptime();
        match (cpu_offset, cpu_data, uptime_offset, uptime_data) {
            (Some(cpu_offset), Ok(cpu_data), Some(uptime_offset), Ok(uptime_data)) => {
                Self::generate_cpu_utilization_enhanced_metrics(
                    &cpu_offset,
                    &cpu_data,
                    uptime_offset,
                    uptime_data,
                    &aggr_handle,
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
        aggr: &AggregatorHandle,
        tags: Option<SortedTags>,
    ) {
        let now = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let metric = Metric::new(
            constants::TMP_MAX_METRIC.into(),
            MetricValue::distribution(tmp_max),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert tmp_max metric: {}", e);
        }

        let metric = Metric::new(
            constants::TMP_USED_METRIC.into(),
            MetricValue::distribution(tmp_used),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert tmp_used metric: {}", e);
        }

        let tmp_free = tmp_max - tmp_used;
        let metric = Metric::new(
            constants::TMP_FREE_METRIC.into(),
            MetricValue::distribution(tmp_free),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert tmp_free metric: {}", e);
        }
    }

    pub fn set_tmp_enhanced_metrics(&self, mut send_metrics: Receiver<()>) {
        if !self.config.enhanced_metrics {
            return;
        }

        let aggr = self.aggr_handle.clone();
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

            let mut interval = interval(Duration::from_millis(constants::MONITORING_INTERVAL));
            loop {
                tokio::select! {
                    biased;
                    // When the stop signal is received, generate final metrics
                    _ = send_metrics.changed() => {
                        Self::generate_tmp_enhanced_metrics(tmp_max, tmp_used, &aggr, tags);
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

    pub fn generate_process_metrics(
        fd_max: f64,
        fd_use: f64,
        threads_max: f64,
        threads_use: f64,
        aggr: &AggregatorHandle,
        tags: Option<SortedTags>,
    ) {
        let now = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let metric = Metric::new(
            constants::FD_MAX_METRIC.into(),
            MetricValue::distribution(fd_max),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert fd_max metric: {}", e);
        }

        // Check if fd_use value is valid before inserting metric
        if fd_use > 0.0 {
            let metric = Metric::new(
                constants::FD_USE_METRIC.into(),
                MetricValue::distribution(fd_use),
                tags.clone(),
                Some(now),
            );
            if let Err(e) = aggr.insert_batch(vec![metric]) {
                error!("Failed to insert fd_use metric: {}", e);
            }
        } else {
            debug!("Could not get file descriptor usage data.");
        }

        let metric = Metric::new(
            constants::THREADS_MAX_METRIC.into(),
            MetricValue::distribution(threads_max),
            tags.clone(),
            Some(now),
        );
        if let Err(e) = aggr.insert_batch(vec![metric]) {
            error!("Failed to insert threads_max metric: {}", e);
        }

        // Check if threads_use value is valid before inserting metric
        if threads_use > 0.0 {
            let metric = Metric::new(
                constants::THREADS_USE_METRIC.into(),
                MetricValue::distribution(threads_use),
                tags,
                Some(now),
            );
            if let Err(e) = aggr.insert_batch(vec![metric]) {
                error!("Failed to insert threads_use metric: {}", e);
            }
        } else {
            debug!("Could not get thread usage data.");
        }
    }

    pub fn set_process_enhanced_metrics(&self, mut send_metrics: Receiver<()>) {
        if !self.config.enhanced_metrics {
            return;
        }

        let aggr = self.aggr_handle.clone();
        let tags = self.get_dynamic_value_tags();

        tokio::spawn(async move {
            // get list of all process ids
            let mut pids = proc::get_pid_list();

            // Set fd_max and initial value for fd_use
            let fd_max = proc::get_fd_max_data(&pids);
            let mut fd_use = proc::get_fd_use_data(&pids);

            // Set threads_max and initial value for threads_use
            let threads_max = proc::get_threads_max_data(&pids);
            let mut threads_use = proc::get_threads_use_data(&pids).unwrap_or_else(|_| -1_f64);

            let mut interval = interval(Duration::from_millis(constants::MONITORING_INTERVAL));
            loop {
                tokio::select! {
                    biased;
                    // When the stop signal is received, generate final metrics
                    _ = send_metrics.changed() => {
                        Self::generate_process_metrics(fd_max, fd_use, threads_max, threads_use, &aggr, tags.clone());
                        return;
                    }
                    // Otherwise keep monitoring file descriptor and thread usage periodically
                    _ = interval.tick() => {
                        pids = proc::get_pid_list();
                        let fd_use_curr = proc::get_fd_use_data(&pids);
                        fd_use = fd_use.max(fd_use_curr);
                        if let Ok(threads_use_curr) = proc::get_threads_use_data(&pids) {
                            threads_use = threads_use.max(threads_use_curr);
                        }
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

    pub fn set_report_log_metrics(&self, metrics: &ReportMetrics, timestamp: i64) {
        if !self.config.enhanced_metrics {
            return;
        }
        let metric = metric::Metric::new(
            constants::DURATION_METRIC.into(),
            MetricValue::distribution(metrics.duration_ms * constants::MS_TO_SEC),
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::BILLED_DURATION_METRIC.into(),
            MetricValue::distribution(metrics.billed_duration_ms as f64 * constants::MS_TO_SEC),
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert billed duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MAX_MEMORY_USED_METRIC.into(),
            MetricValue::distribution(metrics.max_memory_used_mb as f64),
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert max memory used metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MEMORY_SIZE_METRIC.into(),
            MetricValue::distribution(metrics.memory_size_mb as f64),
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
            error!("failed to insert memory size metric: {}", e);
        }

        let cost_usd =
            Self::calculate_estimated_cost_usd(metrics.billed_duration_ms, metrics.memory_size_mb);
        let metric = metric::Metric::new(
            constants::ESTIMATED_COST_METRIC.into(),
            MetricValue::distribution(cost_usd),
            self.get_dynamic_value_tags(),
            Some(timestamp),
        );
        if let Err(e) = self.aggr_handle.insert_batch(vec![metric]) {
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
    use dogstatsd::aggregator_service::AggregatorService;
    use dogstatsd::metric::EMPTY_TAGS;
    const PRECISION: f64 = 0.000_000_01;

    fn setup() -> (AggregatorHandle, Arc<config::Config>) {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let (service, handle) =
            AggregatorService::new(EMPTY_TAGS, 1024).expect("failed to create aggregator service");

        tokio::spawn(service.run());

        (handle, config)
    }

    async fn assert_sketch(handle: &AggregatorHandle, metric_id: &str, value: f64, timestamp: i64) {
        assert_sketch_with_tag(handle, metric_id, value, timestamp, None).await;
    }

    async fn assert_sketch_with_tag(
        handle: &AggregatorHandle,
        metric_id: &str,
        value: f64,
        timestamp: i64,
        tags: Option<SortedTags>,
    ) {
        let ts = (timestamp / 10) * 10;
        if let Some(e) = handle
            .get_entry_by_id(metric_id.into(), tags, ts)
            .await
            .unwrap()
        {
            let metric = e.value.get_sketch().unwrap();
            assert!((metric.max().unwrap() - value).abs() < PRECISION);
            assert!((metric.min().unwrap() - value).abs() < PRECISION);
            assert!((metric.sum().unwrap() - value).abs() < PRECISION);
            assert!((metric.avg().unwrap() - value).abs() < PRECISION);
        } else {
            panic!("{}", format!("{metric_id} not found"));
        }
    }

    #[tokio::test]
    #[allow(clippy::float_cmp)]
    async fn test_increment_invocation_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        lambda.increment_invocation_metric(now);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        assert_sketch(&metrics_aggr, constants::INVOCATIONS_METRIC, 1f64, now).await;
    }

    #[tokio::test]
    #[allow(clippy::float_cmp)]
    async fn test_increment_errors_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        lambda.increment_errors_metric(now);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        assert_sketch(&metrics_aggr, constants::ERRORS_METRIC, 1f64, now).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_disabled() {
        let (metrics_aggr, no_config) = setup();
        let my_config = Arc::new(config::Config {
            enhanced_metrics: false,
            ..no_config.as_ref().clone()
        });
        let mut lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        lambda.increment_invocation_metric(now);
        lambda.increment_errors_metric(now);
        lambda.increment_timeout_metric(now);
        lambda.set_init_duration_metric(InitType::OnDemand, 100.0, now);
        lambda.set_runtime_done_metrics(
            &RuntimeDoneMetrics {
                duration_ms: 100.0,
                produced_bytes: Some(42_u64),
            },
            now,
        );
        lambda.set_post_runtime_duration_metric(100.0, now);
        lambda.set_report_log_metrics(
            &ReportMetrics {
                duration_ms: 100.0,
                billed_duration_ms: 100,
                max_memory_used_mb: 128,
                memory_size_mb: 256,
                init_duration_ms: Some(50.0),
                restore_duration_ms: None,
            },
            now,
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::INVOCATIONS_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::ERRORS_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::TIMEOUTS_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::INIT_DURATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::RUNTIME_DURATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::PRODUCED_BYTES_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::POST_RUNTIME_DURATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::DURATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::BILLED_DURATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::MAX_MEMORY_USED_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::MEMORY_SIZE_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::ESTIMATED_COST_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::RX_BYTES_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::TX_BYTES_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::TOTAL_NETWORK_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::CPU_USER_TIME_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::CPU_SYSTEM_TIME_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::CPU_TOTAL_TIME_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(
                    constants::CPU_TOTAL_UTILIZATION_PCT_METRIC.into(),
                    None,
                    now
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::CPU_TOTAL_UTILIZATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::NUM_CORES_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::CPU_MIN_UTILIZATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::CPU_MAX_UTILIZATION_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::TMP_MAX_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::TMP_USED_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::TMP_FREE_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::FD_MAX_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::FD_USE_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::THREADS_MAX_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::THREADS_USE_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_set_runtime_done_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let runtime_done_metrics = RuntimeDoneMetrics {
            duration_ms: 100.0,
            produced_bytes: Some(42_u64),
        };
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        lambda.set_runtime_done_metrics(&runtime_done_metrics, now);

        assert_sketch(
            &metrics_aggr,
            constants::RUNTIME_DURATION_METRIC,
            100.0,
            now,
        )
        .await;
        assert_sketch(&metrics_aggr, constants::PRODUCED_BYTES_METRIC, 42.0, now).await;
    }

    #[tokio::test]
    async fn test_set_report_log_metrics() {
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
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        lambda.set_report_log_metrics(&report_metrics, now);

        assert_sketch(&metrics_aggr, constants::DURATION_METRIC, 0.1, now).await;
        assert_sketch(&metrics_aggr, constants::BILLED_DURATION_METRIC, 0.1, now).await;

        assert_sketch(&metrics_aggr, constants::MAX_MEMORY_USED_METRIC, 128.0, now).await;
        assert_sketch(&metrics_aggr, constants::MEMORY_SIZE_METRIC, 256.0, now).await;
    }

    #[tokio::test]
    async fn test_set_network_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let _lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
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
            &metrics_aggr,
            None,
        );

        assert_sketch(&metrics_aggr, constants::RX_BYTES_METRIC, 20000.0, now).await;
        assert_sketch(&metrics_aggr, constants::TX_BYTES_METRIC, 74746.0, now).await;
        assert_sketch(&metrics_aggr, constants::TOTAL_NETWORK_METRIC, 94746.0, now).await;
    }

    #[tokio::test]
    async fn test_set_cpu_time_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let _lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
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

        Lambda::generate_cpu_time_enhanced_metrics(&cpu_offset, &cpu_data, &metrics_aggr, None);

        assert_sketch(&metrics_aggr, constants::CPU_USER_TIME_METRIC, 100.0, now).await;
        assert_sketch(&metrics_aggr, constants::CPU_SYSTEM_TIME_METRIC, 53.0, now).await;
        assert_sketch(&metrics_aggr, constants::CPU_TOTAL_TIME_METRIC, 153.0, now).await;
    }

    #[tokio::test]
    async fn test_set_cpu_utilization_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let _lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
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
            &metrics_aggr,
            None,
        );

        // the differences above and metric values below are from an invocation using the go agent to verify the calculations
        assert_sketch(
            &metrics_aggr,
            constants::CPU_TOTAL_UTILIZATION_PCT_METRIC,
            30.0,
            now,
        )
        .await;
        assert_sketch(
            &metrics_aggr,
            constants::CPU_TOTAL_UTILIZATION_METRIC,
            0.6,
            now,
        )
        .await;
        assert_sketch(&metrics_aggr, constants::NUM_CORES_METRIC, 2.0, now).await;
        assert_sketch(
            &metrics_aggr,
            constants::CPU_MAX_UTILIZATION_METRIC,
            30.0,
            now,
        )
        .await;
        assert_sketch(
            &metrics_aggr,
            constants::CPU_MIN_UTILIZATION_METRIC,
            28.75,
            now,
        )
        .await;
    }

    #[tokio::test]
    async fn test_set_tmp_enhanced_metrics() {
        let (metrics_aggr, my_config) = setup();
        let _lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let tmp_max = 550_461_440.0;
        let tmp_used = 12_165_120.0;

        Lambda::generate_tmp_enhanced_metrics(tmp_max, tmp_used, &metrics_aggr, None);

        assert_sketch(&metrics_aggr, constants::TMP_MAX_METRIC, 550_461_440.0, now).await;
        assert_sketch(&metrics_aggr, constants::TMP_USED_METRIC, 12_165_120.0, now).await;
        assert_sketch(
            &metrics_aggr,
            constants::TMP_FREE_METRIC,
            538_296_320.0,
            now,
        )
        .await;
    }

    #[tokio::test]
    async fn test_set_process_enhanced_metrics_valid_use() {
        let (metrics_aggr, my_config) = setup();
        let _lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let fd_max = 1024.0;
        let fd_use = 175.0;
        let threads_max = 1024.0;
        let threads_use = 40.0;

        Lambda::generate_process_metrics(
            fd_max,
            fd_use,
            threads_max,
            threads_use,
            &metrics_aggr,
            None,
        );

        assert_sketch(&metrics_aggr, constants::FD_MAX_METRIC, 1024.0, now).await;
        assert_sketch(&metrics_aggr, constants::FD_USE_METRIC, 175.0, now).await;
        assert_sketch(&metrics_aggr, constants::THREADS_MAX_METRIC, 1024.0, now).await;
        assert_sketch(&metrics_aggr, constants::THREADS_USE_METRIC, 40.0, now).await;
    }

    #[tokio::test]
    async fn test_set_process_enhanced_metrics_invalid_use() {
        let (metrics_aggr, my_config) = setup();
        let _lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let fd_max = 1024.0;
        let fd_use = -1.0;
        let threads_max = 1024.0;
        let threads_use = -1.0;

        Lambda::generate_process_metrics(
            fd_max,
            fd_use,
            threads_max,
            threads_use,
            &metrics_aggr,
            None,
        );

        assert_sketch(&metrics_aggr, constants::FD_MAX_METRIC, 1024.0, now).await;
        assert_sketch(&metrics_aggr, constants::THREADS_MAX_METRIC, 1024.0, now).await;

        assert!(
            metrics_aggr
                .get_entry_by_id(constants::FD_USE_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            metrics_aggr
                .get_entry_by_id(constants::THREADS_USE_METRIC.into(), None, now)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_set_config_load_issue_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let test_reason = "test_config_issue";

        lambda.set_config_load_issue_metric(now, test_reason);

        // Create the expected tags for the metric lookup
        let expected_tags = SortedTags::parse(&format!("reason:{test_reason}")).ok();
        assert_sketch_with_tag(
            &metrics_aggr,
            constants::DATADOG_SERVERLESS_EXTENSION_FAILOVER_CONFIG_ISSUE_METRIC,
            1f64,
            now,
            expected_tags,
        )
        .await;
    }
}
