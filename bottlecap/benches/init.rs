//! Init pieces that run in-process: the `Ustr` first use (measured in a child process, since
//! the interner is global) and the HTTP client builds.
//!
//! Review items: 3.1, 3.5, 3.6, H15.
//!
//! With `BOTTLECAP_BENCH_CHILD=<step>` the binary runs one step in a fresh process, prints the
//! elapsed nanoseconds on stdout and exits. The child builds the runtime first, as `main` does.
//! The `first_*` benches sum those child timings with `iter_custom`; the process spawn is not
//! measured.

mod common;
#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

use std::hint::black_box;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bottlecap::config::Config;
use criterion::{BatchSize, Criterion, SamplingMode, criterion_group};
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use dogstatsd::metric::SortedTags;
use fixtures::dogstatsd::ENRICH_TAGS;
use ustr::Ustr;

const CHILD_ENV: &str = "BOTTLECAP_BENCH_CHILD";
/// When set, the child prints its RSS in KiB before and after the step instead of the time.
const CHILD_RSS_ENV: &str = "BOTTLECAP_BENCH_CHILD_RSS";

const USTR_FIRST_USE: &str = "ustr_first_use";
const SORTED_TAGS_FIRST_PARSE: &str = "sorted_tags_first_parse";
const FIRST_REGISTER_CLIENT: &str = "first_register_client";
const FIRST_CLIENT_SEQUENCE: &str = "first_client_sequence";

/// the register client in `src/bin/bottlecap/main.rs`.
fn register_client() -> reqwest::Client {
    create_reqwest_client_builder()
        .expect("client builder")
        .no_proxy()
        .build()
        .expect("register client")
}

/// `build_trace_agent` in `src/startup.rs`.
fn trace_client(config: &Config) -> impl Sized {
    bottlecap::traces::http_client::create_client(
        config.proxy_https.as_ref(),
        config.tls_cert_file.as_ref(),
        config.skip_ssl_validation,
    )
    .expect("trace client")
}

fn rss_kib() -> u64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("ps prints the RSS")
}

fn run_step(step: &str, config: &Arc<Config>) {
    match step {
        // `init_ustr` in `src/bin/bottlecap/main.rs`.
        USTR_FIRST_USE => {
            black_box(Ustr::from(""));
        }
        // The first interner use when `init_ustr` has not finished: `start_dogstatsd` parses
        // the enrichment tags.
        SORTED_TAGS_FIRST_PARSE => {
            black_box(SortedTags::parse(ENRICH_TAGS).expect("enrichment tags"));
        }
        FIRST_REGISTER_CLIENT => {
            black_box(register_client());
        }
        // The three client builds of a cold start, in order: register, shared `http::get_client`,
        // and the trace client from `build_trace_agent`.
        FIRST_CLIENT_SEQUENCE => {
            black_box(register_client());
            black_box(bottlecap::http::get_client(config));
            black_box(trace_client(config));
        }
        other => panic!("unknown child step {other}"),
    }
}

fn child(step: &str) -> ! {
    let rt = common::runtime();
    let _guard = rt.enter();
    let config = common::config();
    bottlecap::fips::prepare_client_provider().expect("crypto provider");
    if std::env::var_os(CHILD_RSS_ENV).is_some() {
        let before = rss_kib();
        run_step(step, &config);
        println!("{before} {}", rss_kib());
    } else {
        let start = Instant::now();
        run_step(step, &config);
        println!("{}", start.elapsed().as_nanos());
    }
    std::process::exit(0);
}

fn spawn_child(step: &str, rss: bool) -> String {
    let mut cmd = Command::new(std::env::current_exe().expect("current exe"));
    cmd.env(CHILD_ENV, step);
    if rss {
        cmd.env(CHILD_RSS_ENV, "1");
    }
    let out = cmd.output().expect("spawn child");
    assert!(
        out.status.success(),
        "child {step} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 child output")
}

fn print_child_rss(group: &str, step: &str) {
    let out = spawn_child(step, true);
    let mut kib = out
        .split_whitespace()
        .map(|v| v.parse::<i64>().expect("KiB"));
    let (before, after) = (kib.next().expect("before"), kib.next().expect("after"));
    println!(
        "{group}: child RSS around {step}: {before} KiB before, {after} KiB after ({:+} KiB)",
        after - before
    );
}

/// Benches `step` in fresh child processes, in its own group `group` so the short sample
/// settings do not leak into the in-process benches. One spawn per iteration, so the counts
/// stay small; criterion's "Unable to complete 10 samples in 20.0ms" warning is expected.
fn bench_child(c: &mut Criterion, group: &str, id: &str, step: &str) {
    let mut g = c.benchmark_group(group);
    g.sample_size(10)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(20))
        .sampling_mode(SamplingMode::Flat);
    g.bench_function(id, |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    let ns = spawn_child(step, false)
                        .trim()
                        .parse()
                        .expect("nanoseconds");
                    Duration::from_nanos(ns)
                })
                .sum()
        });
    });
    g.finish();
}

fn ustr(c: &mut Criterion) {
    let mut g = c.benchmark_group("init/ustr");

    let existing: Vec<&str> = ENRICH_TAGS.split(',').collect();
    for s in &existing {
        Ustr::from(s);
    }
    g.bench_function("from_existing_x16", |b| {
        b.iter(|| {
            for s in &existing {
                black_box(Ustr::from(black_box(*s)));
            }
        });
    });

    g.finish();

    print_child_rss("init/ustr", USTR_FIRST_USE);
    print_child_rss("init/ustr", SORTED_TAGS_FIRST_PARSE);
    bench_child(c, "init/ustr", "first_use", USTR_FIRST_USE);
    bench_child(
        c,
        "init/ustr",
        "first_sorted_tags_parse",
        SORTED_TAGS_FIRST_PARSE,
    );
}

fn http_client(c: &mut Criterion) {
    let rt = common::runtime();
    let _guard = rt.enter();
    let config = common::config();
    let config_http1 = Arc::new(Config {
        http_protocol: Some("http1".to_string()),
        ..(*config).clone()
    });
    bottlecap::fips::prepare_client_provider().expect("crypto provider");

    // Warm builds: the second and later client in a process.
    let mut g = c.benchmark_group("init/http_client");
    // `http::get_client` as `main` calls it, HTTP/2 by default (no proxy).
    g.bench_function("shared_get_client", |b| {
        b.iter(|| bottlecap::http::get_client(&config));
    });
    g.bench_function("shared_get_client_http1", |b| {
        b.iter(|| bottlecap::http::get_client(&config_http1));
    });
    g.bench_function("register_client", |b| b.iter(register_client));
    g.bench_function("trace_client", |b| b.iter(|| trace_client(&config)));

    g.finish();

    // Cold builds: the first client in a fresh process pays the TLS and lazy-static setup.
    print_child_rss("init/http_client", FIRST_CLIENT_SEQUENCE);
    bench_child(
        c,
        "init/http_client",
        "first_register_client",
        FIRST_REGISTER_CLIENT,
    );
    bench_child(
        c,
        "init/http_client",
        "first_client_sequence",
        FIRST_CLIENT_SEQUENCE,
    );
}

fn tokio_runtime(c: &mut Criterion) {
    // `#[tokio::main]` on `main`; the drop (worker shutdown) is outside the measurement.
    let mut g = c.benchmark_group("init/tokio_runtime/build");
    for workers in [2, 4] {
        g.bench_function(workers.to_string(), |b| {
            b.iter_batched(
                || (),
                |()| {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(workers)
                        .enable_all()
                        .build()
                        .expect("tokio runtime")
                },
                BatchSize::PerIteration,
            );
        });
    }
    g.finish();
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = ustr, http_client, tokio_runtime
}

fn main() {
    if let Ok(step) = std::env::var(CHILD_ENV) {
        child(&step);
    }
    benches();
    common::criterion().final_summary();
}
