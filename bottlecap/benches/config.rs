//! Config load: `get_config` with the none, typical and heavy env sets, and the APM
//! replace-rule compile.
//!
//! Review items: 1, 3.4, 3.8.
//!
//! No tracing subscriber is installed, so the config warnings cost only their level check. The
//! extension keeps the config and the compiled rules, so their drop is outside the measurement.

mod common;
#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

use std::hint::black_box;

use bottlecap::config::apm_replace_rule::deserialize_apm_replace_rules;
use bottlecap::config::processing_rule::ProcessingRule;
use bottlecap::logs::lambda::IntakeLog;
use bottlecap::logs::lambda::processor::LambdaProcessor;
use bottlecap::logs::processor::Processor;
use criterion::{Criterion, criterion_group, criterion_main};
use fixtures::invocation::{ENV, REGION, SERVICE, VERSION};
use serde::de::value::{Error as DeError, StrDeserializer};

// The rest of the environment Lambda gives an extension (`common::setup_env` sets the function
// identity), without any DD_* variable. PATH is left out so the bench keeps the host PATH. The
// variables Lambda gives only to the runtime process (`AWS_EXECUTION_ENV`, `LAMBDA_TASK_ROOT`,
// `_HANDLER`, the log group and X-Ray ones; see the Extensions API docs) and the per-invocation
// `_X_AMZN_TRACE_ID` are left out.
const LAMBDA_BASE: &[(&str, &str)] = &[
    ("AWS_LAMBDA_LOG_FORMAT", "Text"),
    ("AWS_LAMBDA_RUNTIME_API", "127.0.0.1:9001"),
    ("AWS_DEFAULT_REGION", REGION),
    ("AWS_ACCESS_KEY_ID", "ASIAEXAMPLEEXAMPLE12"),
    (
        "AWS_SECRET_ACCESS_KEY",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    ),
    (
        "AWS_SESSION_TOKEN",
        "IQoJb3JpZ2luX2VjEJr//////////wEaCXVzLWVhc3QtMSJHMEUCIQDexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexampleexample",
    ),
    (
        "LD_LIBRARY_PATH",
        "/var/lang/lib:/lib64:/usr/lib64:/var/runtime:/var/runtime/lib:/var/task:/var/task/lib:/opt/lib",
    ),
    ("LANG", "en_US.UTF-8"),
    ("TZ", ":UTC"),
    ("AWS_LAMBDA_EXEC_WRAPPER", "/opt/datadog_wrapper"),
    ("SHLVL", "0"),
];

// A typical Datadog Lambda setup, as the Serverless Framework / CDK plugins generate it.
const TYPICAL: &[(&str, &str)] = &[
    (
        "DD_API_KEY_SECRET_ARN",
        "arn:aws:secretsmanager:us-east-1:123456789012:secret:dd-api-key-AbCdEf",
    ),
    ("DD_SITE", "datadoghq.com"),
    ("DD_ENV", ENV),
    ("DD_SERVICE", SERVICE),
    ("DD_VERSION", VERSION),
    ("DD_TAGS", "team:payments,tier:backend"),
    ("DD_LOG_LEVEL", "debug"),
    ("DD_TRACE_ENABLED", "true"),
    ("DD_SERVERLESS_LOGS_ENABLED", "true"),
    ("DD_LAMBDA_HANDLER", "app.handler"),
    ("DD_FLUSH_TO_LOG", "false"),
    ("DD_MERGE_XRAY_TRACES", "false"),
    ("DD_CAPTURE_LAMBDA_PAYLOAD", "false"),
    ("DD_ENHANCED_METRICS", "true"),
    ("DD_TRACE_OTEL_ENABLED", "false"),
    ("DD_LOGS_INJECTION", "true"),
    ("DD_SERVERLESS_FLUSH_STRATEGY", "end"),
    ("DD_EXTENSION_VERSION", "next"),
    ("DD_PROFILING_ENABLED", "false"),
    ("DD_SERVERLESS_APPSEC_ENABLED", "false"),
];

const ENDPOINTS_JSON: &str = r#"{"https://app.datadoghq.eu":["0123456789abcdef0123456789abcde1","0123456789abcdef0123456789abcde2"],"https://app.us5.datadoghq.com":["0123456789abcdef0123456789abcde3"]}"#;

const PROCESSING_RULES_3: &str = r#"[{"type":"exclude_at_match","name":"exclude_health","pattern":"GET /health"},{"type":"mask_sequences","name":"mask_cc","pattern":"\\d{4}-\\d{4}-\\d{4}-\\d{4}","replace_placeholder":"[REDACTED]"},{"type":"include_at_match","name":"inc","pattern":"ERROR|WARN"}]"#;

const REPLACE_TAGS_1: &str = r#"[{"name":"http.url","pattern":"(token=)[^&]*","repl":"${1}?"}]"#;
const REPLACE_TAGS_3: &str = r#"[{"name":"http.url","pattern":"(token=)[^&]*","repl":"${1}?"},{"name":"resource.name","pattern":"/users/\\d+","repl":"/users/?"},{"name":"*","pattern":"\\d{16}","repl":"?"}]"#;

// A heavy setup: processing rules, dual shipping, APM replace rules, filters, OTLP, DSM.
const HEAVY_EXTRA: &[(&str, &str)] = &[
    ("DD_LOGS_CONFIG_PROCESSING_RULES", PROCESSING_RULES_3),
    ("DD_ADDITIONAL_ENDPOINTS", ENDPOINTS_JSON),
    ("DD_APM_ADDITIONAL_ENDPOINTS", ENDPOINTS_JSON),
    (
        "DD_LOGS_CONFIG_ADDITIONAL_ENDPOINTS",
        r#"[{"api_key":"0123456789abcdef0123456789abcde4","Host":"agent-http-intake.logs.datadoghq.eu","Port":443,"is_reliable":true}]"#,
    ),
    ("DD_APM_REPLACE_TAGS", REPLACE_TAGS_3),
    (
        "DD_SERVICE_MAPPING",
        "lambda_sqs:my-queue,lambda_sns:my-topic",
    ),
    ("DD_APM_FILTER_TAGS_REQUIRE", "env:prod"),
    ("DD_APM_FILTER_TAGS_REJECT", "debug:true"),
    ("DD_TRACE_PROPAGATION_STYLE", "datadog,tracecontext,b3multi"),
    ("DD_PROXY_HTTPS", "http://proxy.internal:3128"),
    ("DD_PROXY_NO_PROXY", "169.254.169.254,localhost"),
    (
        "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
        "localhost:4318",
    ),
    ("DD_OTLP_CONFIG_TRACES_SPAN_NAME_REMAPPINGS", "a:b,c:d"),
    ("DD_LAMBDA_CUSTOMER_METRICS_EXCLUDE_TAGS", "requestid,host"),
    ("DD_SPAN_DEDUP_TIMEOUT", "5"),
    ("DD_APPSEC_WAF_TIMEOUT", "5000"),
    ("DD_DATA_STREAMS_ENABLED", "true"),
    ("DD_COMPRESSION_LEVEL", "6"),
    ("DD_FLUSH_TIMEOUT", "10"),
    ("DD_LOGS_CONFIG_USE_COMPRESSION", "true"),
    ("DD_APM_FEATURES", "enable_stats_by_span_kind"),
    ("DD_LAMBDA_EXTENSION_COMPUTE_STATS", "true"),
    ("DD_TRACE_STATS_ADDITIONAL_TAGS", "http.status_code"),
    ("DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED", "true"),
    ("DD_API_KEY_SECRET_RELOAD_INTERVAL", "3600"),
    ("DD_LAMBDA_DURABLE_FUNCTION_LOG_BUFFER_SIZE", "10"),
];

fn set_all(pairs: &[(&str, &str)]) {
    for (k, v) in pairs {
        // Benches are single-threaded at setup time.
        unsafe { std::env::set_var(k, v) };
    }
}

/// Removes every `DD_*` and `DATADOG_*` variable, including any from the host shell.
fn clear_dd_env() {
    let keys: Vec<_> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| {
            k.to_str()
                .is_some_and(|k| k.starts_with("DD_") || k.starts_with("DATADOG_"))
        })
        .collect();
    for k in keys {
        unsafe { std::env::remove_var(k) };
    }
}

fn set_scenario(name: &str) {
    clear_dd_env();
    common::setup_env();
    set_all(LAMBDA_BASE);
    match name {
        "none" => {}
        "typical" => set_all(TYPICAL),
        "heavy" => {
            set_all(TYPICAL);
            set_all(HEAVY_EXTRA);
        }
        // Isolates the replace-rule compile inside `heavy`.
        "heavy_no_replace_tags" => {
            set_all(TYPICAL);
            set_all(HEAVY_EXTRA);
            unsafe { std::env::remove_var("DD_APM_REPLACE_TAGS") };
        }
        other => panic!("unknown scenario {other}"),
    }
}

fn get_config(c: &mut Criterion) {
    let mut g = c.benchmark_group("config/get_config");
    // `main` reads `$LAMBDA_TASK_ROOT/datadog.yaml` (`/var/task` when unset, as for an
    // extension); here the file does not exist.
    let dir = tempfile::tempdir().expect("tempdir");
    for scenario in ["none", "typical", "heavy", "heavy_no_replace_tags"] {
        set_scenario(scenario);
        println!(
            "config/get_config/{scenario}: {} env vars in the process",
            std::env::vars_os().count()
        );
        g.bench_function(scenario, |b| {
            b.iter_with_large_drop(|| bottlecap::config::get_config(black_box(dir.path())));
        });
    }
    clear_dd_env();
    g.finish();
}

fn replace_rules(c: &mut Criterion) {
    // `DD_APM_REPLACE_TAGS` is compiled once, inside `get_config`: the env source's field uses
    // `deserialize_apm_replace_rules` (datadog-agent-config `deserializers/apm_replace_rule.rs`),
    // which validates the JSON, then calls `libdd_trace_obfuscation::replacer::
    // parse_rules_from_string` (one `Regex::new` per rule). `build_trace_agent` only clones the
    // compiled rules into `ObfuscationConfig`.
    let mut g = c.benchmark_group("config/replace_rules/compile");
    for (n, json) in [("1", REPLACE_TAGS_1), ("3", REPLACE_TAGS_3)] {
        g.bench_function(n, |b| {
            b.iter_with_large_drop(|| {
                deserialize_apm_replace_rules(StrDeserializer::<DeError>::new(black_box(json)))
                    .expect("replace rules")
            });
        });
    }
    g.finish();
}

fn processing_rules(c: &mut Criterion) {
    // `LambdaProcessor::new` (`src/logs/lambda/processor.rs`) compiles the rules with the
    // `Processor::compile_rules` default method (`src/logs/processor.rs`).
    let rules: Option<Vec<ProcessingRule>> =
        Some(serde_json::from_str(PROCESSING_RULES_3).expect("processing rules"));
    let mut g = c.benchmark_group("config/processing_rules/compile");
    g.bench_function("3", |b| {
        b.iter_with_large_drop(|| {
            <LambdaProcessor as Processor<IntakeLog>>::compile_rules(black_box(&rules))
        });
    });
    g.finish();
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = get_config, replace_rules, processing_rules
}
criterion_main!(benches);
