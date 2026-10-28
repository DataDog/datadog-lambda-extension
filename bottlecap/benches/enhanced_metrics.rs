//! Enhanced metrics: `set_max_enhanced_metrics` (2 regex compiles + `/proc/<pid>/limits`),
//! the usage-monitor tick (`/proc` readers), the per-invocation `/proc` reads and the
//! dynamic tag rebuild.
//!
//! Review items: 5.0, 5.8, 5.9, 5.21.
//!
//! The `*_real_proc` and `set_max/fd_and_threads_max` benches read the live `/proc` and run only
//! on Linux. The `*_fixture` and `fake_proc_tree` variants read `tests/proc` or a temp tree, so
//! they run everywhere.

mod common;
#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

use std::collections::HashMap;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::{self, BufRead};
use std::path::Path;

use bottlecap::metrics::enhanced::lambda::Lambda;
use bottlecap::metrics::enhanced::statfs;
use bottlecap::proc::{self, constants};
use criterion::{Criterion, criterion_group, criterion_main};
use dogstatsd::aggregator::AggregatorService;
use dogstatsd::metric::{EMPTY_TAGS, Metric, MetricValue, SortedTags};
use regex::Regex;

const PROC_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/proc");
const LIMITS_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/proc/process/valid");
const LIMITS_PIDS: [i64; 2] = [9, 31];

// `get_fd_max_data_from_path` and `get_threads_max_data_from_path` in `src/proc/mod.rs`.
const FD_MAX_RE: &str = r"^Max open files\s+(\d+)";
const THREADS_MAX_RE: &str = r"^Max processes\s+(\d+)";

fn skip_unless_linux(id: &str) -> bool {
    if cfg!(target_os = "linux") {
        return false;
    }
    println!("enhanced_metrics/{id}: skipped, reads the live /proc (Linux only)");
    true
}

/// `get_fd_max_data_from_path` with the regex compiled once by the caller (PR #1278).
fn limits_min_hoisted(root: &str, pids: &[i64], re: &Regex, default: f64) -> f64 {
    let mut min = default;
    for &pid in pids {
        let Ok(file) = File::open(format!("{root}/{pid}/limits")) else {
            continue;
        };
        for line in io::BufReader::new(file).lines().map_while(Result::ok) {
            if let Some(caps) = re.captures(&line) {
                if let Ok(v) = caps[1].parse::<f64>() {
                    min = min.min(v);
                }
                break;
            }
        }
    }
    min
}

/// The no-regex alternative: one read per file and `strip_prefix` (review 5.9).
fn limits_min_strip_prefix(root: &str, pids: &[i64], prefix: &str, default: f64) -> f64 {
    let mut min = default;
    for &pid in pids {
        let Ok(s) = fs::read_to_string(format!("{root}/{pid}/limits")) else {
            continue;
        };
        if let Some(v) = s
            .lines()
            .find_map(|l| l.strip_prefix(prefix))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<f64>().ok())
        {
            min = min.min(v);
        }
    }
    min
}

fn set_max(c: &mut Criterion) {
    let mut g = c.benchmark_group("enhanced_metrics/set_max");

    // `Lambda::set_max_enhanced_metrics`, `src/metrics/enhanced/lambda.rs`.
    if !skip_unless_linux("set_max/fd_and_threads_max") {
        g.bench_function("fd_and_threads_max", |b| {
            b.iter(|| {
                let pids = proc::get_pid_list();
                (
                    proc::get_fd_max_data(&pids),
                    proc::get_threads_max_data(&pids),
                )
            });
        });
    }

    g.bench_function("fd_and_threads_max_fixture", |b| {
        b.iter(|| {
            (
                proc::get_fd_max_data_from_path(LIMITS_FIXTURE, &LIMITS_PIDS),
                proc::get_threads_max_data_from_path(LIMITS_FIXTURE, &LIMITS_PIDS),
            )
        });
    });

    g.bench_function("regex_compile_x2", |b| {
        b.iter(|| {
            (
                Regex::new(black_box(FD_MAX_RE)).expect("fd regex"),
                Regex::new(black_box(THREADS_MAX_RE)).expect("threads regex"),
            )
        });
    });

    let fd_re = Regex::new(FD_MAX_RE).expect("fd regex");
    let threads_re = Regex::new(THREADS_MAX_RE).expect("threads regex");
    g.bench_function("hoisted_regex_fixture", |b| {
        b.iter(|| {
            (
                limits_min_hoisted(
                    LIMITS_FIXTURE,
                    &LIMITS_PIDS,
                    &fd_re,
                    constants::LAMBDA_FILE_DESCRIPTORS_DEFAULT_LIMIT,
                ),
                limits_min_hoisted(
                    LIMITS_FIXTURE,
                    &LIMITS_PIDS,
                    &threads_re,
                    constants::LAMBDA_EXECUTION_PROCESSES_DEFAULT_LIMIT,
                ),
            )
        });
    });

    g.bench_function("no_regex_strip_prefix_fixture", |b| {
        b.iter(|| {
            (
                limits_min_strip_prefix(
                    LIMITS_FIXTURE,
                    &LIMITS_PIDS,
                    "Max open files",
                    constants::LAMBDA_FILE_DESCRIPTORS_DEFAULT_LIMIT,
                ),
                limits_min_strip_prefix(
                    LIMITS_FIXTURE,
                    &LIMITS_PIDS,
                    "Max processes",
                    constants::LAMBDA_EXECUTION_PROCESSES_DEFAULT_LIMIT,
                ),
            )
        });
    });

    // The method itself: pid list, the 2 limits reads, tag rebuild, 2 `Metric::new` and 2
    // aggregator sends. Outside Linux the pid list is empty, so only the regex compiles,
    // the tags and the sends remain.
    let rt = common::runtime();
    let _guard = rt.enter();
    let (service, handle) = AggregatorService::new(EMPTY_TAGS, 1024).expect("aggregator");
    rt.spawn(service.run());
    let mut lambda = Lambda::new(handle, common::config());
    lambda.set_init_tags(false, true);
    lambda.set_runtime_tag("python3.12");
    g.bench_function("lambda_method", |b| {
        b.iter(|| lambda.set_max_enhanced_metrics())
    });

    g.finish();
}

/// A `/proc`-like tree: 4 pids, each with 32 `fd/` entries, 8 `task/` dirs and a `limits`.
fn fake_proc_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let limits = Path::new(LIMITS_FIXTURE).join("9/limits");
    for pid in 101..105 {
        let pid_dir = dir.path().join(pid.to_string());
        fs::create_dir_all(pid_dir.join("fd")).expect("fd dir");
        for fd in 0..32 {
            File::create(pid_dir.join("fd").join(fd.to_string())).expect("fd file");
        }
        for tid in 0..8 {
            fs::create_dir_all(pid_dir.join("task").join((pid * 100 + tid).to_string()))
                .expect("task dir");
        }
        fs::copy(&limits, pid_dir.join("limits")).expect("limits");
    }
    dir
}

fn usage_tick(c: &mut Criterion) {
    let mut g = c.benchmark_group("enhanced_metrics/usage_tick");

    // `start_usage_metrics_task`, `src/metrics/enhanced/lambda.rs`.
    if !skip_unless_linux("usage_tick/real_proc") {
        g.bench_function("real_proc", |b| {
            b.iter(|| {
                let pids = proc::get_pid_list();
                (
                    statfs::get_tmp_used().ok(),
                    proc::get_fd_use_data(&pids),
                    proc::get_threads_use_data(&pids).ok(),
                )
            });
        });
    }

    let tree = fake_proc_tree();
    let root = tree.path().to_str().expect("utf-8 temp path");
    g.bench_function("fake_proc_tree", |b| {
        b.iter(|| {
            let pids = proc::get_pid_list_from_path(root);
            (
                proc::get_fd_use_data_from_path(root, &pids),
                proc::get_threads_use_data_from_path(root, &pids).ok(),
            )
        });
    });

    g.bench_function("statfs_tmp", |b| b.iter(|| statfs::get_tmp_used().ok()));

    g.finish();
}

fn per_invocation_reads(c: &mut Criterion) {
    let mut g = c.benchmark_group("enhanced_metrics/per_invocation_reads");

    let stat = format!("{PROC_FIXTURE}/stat/valid_stat");
    let net_dev = format!("{PROC_FIXTURE}/net/valid_dev");
    let uptime = format!("{PROC_FIXTURE}/uptime/valid_uptime");
    g.bench_function("cpu_stat", |b| {
        b.iter(|| proc::get_cpu_data_from_path(&stat).ok());
    });
    g.bench_function("net_dev", |b| {
        b.iter(|| proc::get_network_data_from_path(&net_dev).ok());
    });
    g.bench_function("uptime", |b| {
        b.iter(|| proc::get_uptime_from_path(&uptime).ok());
    });

    if !skip_unless_linux("per_invocation_reads/*_real_proc") {
        g.bench_function("cpu_stat_real_proc", |b| {
            b.iter(|| proc::get_cpu_data().ok())
        });
        // Off Lambda the interface is missing and this returns an error after the full read.
        g.bench_function("net_dev_real_proc", |b| {
            b.iter(|| proc::get_network_data().ok());
        });
        g.bench_function("uptime_real_proc", |b| b.iter(|| proc::get_uptime().ok()));
    }

    g.finish();
}

fn dynamic_tags(c: &mut Criterion) {
    let mut g = c.benchmark_group("enhanced_metrics/dynamic_tags");

    let tags: HashMap<String, String> = HashMap::from([
        ("cold_start".to_string(), "false".to_string()),
        ("runtime".to_string(), "python3.12".to_string()),
        ("init_type".to_string(), "on-demand".to_string()),
    ]);
    // `Lambda::get_dynamic_value_tags`, `src/metrics/enhanced/lambda.rs`.
    g.bench_function("rebuild_3_tags", |b| {
        b.iter(|| {
            let vec_tags: Vec<String> = tags.iter().map(|(k, v)| format!("{k}:{v}")).collect();
            SortedTags::parse(&vec_tags.join(",")).ok()
        });
    });

    let cached = SortedTags::parse("cold_start:false,runtime:python3.12,init_type:on-demand").ok();
    g.bench_function("cached_clone", |b| b.iter(|| cached.clone()));
    g.finish();

    let mut g = c.benchmark_group("enhanced_metrics/metric_new");
    g.bench_function("distribution", |b| {
        b.iter(|| {
            Metric::new(
                "aws.lambda.enhanced.duration".into(),
                MetricValue::distribution(0.123),
                cached.clone(),
                Some(1_700_000_000),
            )
        });
    });
    g.finish();
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = set_max, usage_tick, per_invocation_reads, dynamic_tags
}
criterion_main!(benches);
