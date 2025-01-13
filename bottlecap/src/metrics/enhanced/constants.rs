// Pricing constants
pub const BASE_LAMBDA_INVOCATION_PRICE: f64 = 0.000_000_2;
pub const X86_LAMBDA_PRICE_PER_GB_SECOND: f64 = 0.000_016_666_7;
pub const ARM_LAMBDA_PRICE_PER_GB_SECOND: f64 = 0.000_013_333_4;
pub const MS_TO_SEC: f64 = 0.001;
pub const MB_TO_GB: f64 = 1_024.0;

// tmp directory path
pub const TMP_PATH: &str = "/tmp/";

// Enhanced metrics
pub const MAX_MEMORY_USED_METRIC: &str = "aws.lambda.enhanced.max_memory_used";
pub const MEMORY_SIZE_METRIC: &str = "aws.lambda.enhanced.memorysize";
pub const RUNTIME_DURATION_METRIC: &str = "aws.lambda.enhanced.runtime_duration";
pub const BILLED_DURATION_METRIC: &str = "aws.lambda.enhanced.billed_duration";
pub const DURATION_METRIC: &str = "aws.lambda.enhanced.duration";
pub const POST_RUNTIME_DURATION_METRIC: &str = "aws.lambda.enhanced.post_runtime_duration";
pub const ESTIMATED_COST_METRIC: &str = "aws.lambda.enhanced.estimated_cost";
pub const INIT_DURATION_METRIC: &str = "aws.lambda.enhanced.init_duration";
pub const RESPONSE_LATENCY_METRIC: &str = "aws.lambda.enhanced.response_latency";
pub const RESPONSE_DURATION_METRIC: &str = "aws.lambda.enhanced.response_duration";
pub const PRODUCED_BYTES_METRIC: &str = "aws.lambda.enhanced.produced_bytes";
pub const OUT_OF_MEMORY_METRIC: &str = "aws.lambda.enhanced.out_of_memory";
pub const TIMEOUTS_METRIC: &str = "aws.lambda.enhanced.timeouts";
pub const ERRORS_METRIC: &str = "aws.lambda.enhanced.errors";
pub const INVOCATIONS_METRIC: &str = "aws.lambda.enhanced.invocations";
pub const RX_BYTES_METRIC: &str = "aws.lambda.enhanced.rx_bytes";
pub const TX_BYTES_METRIC: &str = "aws.lambda.enhanced.tx_bytes";
pub const TOTAL_NETWORK_METRIC: &str = "aws.lambda.enhanced.total_network";
pub const CPU_SYSTEM_TIME_METRIC: &str = "aws.lambda.enhanced.cpu_system_time";
pub const CPU_USER_TIME_METRIC: &str = "aws.lambda.enhanced.cpu_user_time";
pub const CPU_TOTAL_TIME_METRIC: &str = "aws.lambda.enhanced.cpu_total_time";
pub const CPU_TOTAL_UTILIZATION_PCT_METRIC: &str = "aws.lambda.enhanced.cpu_total_utilization_pct";
pub const CPU_TOTAL_UTILIZATION_METRIC: &str = "aws.lambda.enhanced.cpu_total_utilization";
pub const NUM_CORES_METRIC: &str = "aws.lambda.enhanced.num_cores";
pub const CPU_MAX_UTILIZATION_METRIC: &str = "aws.lambda.enhanced.cpu_max_utilization";
pub const CPU_MIN_UTILIZATION_METRIC: &str = "aws.lambda.enhanced.cpu_min_utilization";
pub const TMP_MAX_METRIC: &str = "aws.lambda.enhanced.tmp_max";
pub const TMP_USED_METRIC: &str = "aws.lambda.enhanced.tmp_used";
pub const TMP_FREE_METRIC: &str = "aws.lambda.enhanced.tmp_free";
pub const FD_MAX_METRIC: &str = "aws.lambda.enhanced.fd_max";
pub const FD_USE_METRIC: &str = "aws.lambda.enhanced.fd_use";
pub const THREADS_MAX_METRIC: &str = "aws.lambda.enhanced.threads_max";
pub const THREADS_USE_METRIC: &str = "aws.lambda.enhanced.threads_use";
//pub const ASM_INVOCATIONS_METRIC: &str = "aws.lambda.enhanced.asm.invocations";
pub const ENHANCED_METRICS_ENV_VAR: &str = "DD_ENHANCED_METRICS";

// Monitoring interval for tmp, fd, and threads metrics
pub const MONITORING_INTERVAL: u64 = 10;
