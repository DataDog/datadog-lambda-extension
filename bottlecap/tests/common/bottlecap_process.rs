// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! The real `bottlecap` binary as a child process, pointed at the fakes.
//!
//! `BottlecapProcess::builder(&api).intake(&url).spawn()` starts the binary
//! cargo built for this test run with a base environment where every Datadog
//! URL points at the fake. stdout and stderr go to files in the task root;
//! RSS and CPU come from `/proc` on Linux and `ps` elsewhere. Dropping the
//! value kills the process.
//!
//! The binary is `CARGO_BIN_EXE_bottlecap`, the uplifted `target/debug/bottlecap`.
//! With a target dir shared between worktrees that file is whichever worktree
//! built last, so run these tests with a per-worktree `CARGO_TARGET_DIR`;
//! `log_tail` prints the binary path and mtime so a stale one is visible.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::harness::fake_extensions_api::FakeExtensionsApi;
use crate::harness::fault_intake::CLOSED_PORT_URL;

/// Budget for a debug build to register, subscribe and reach `/next`.
pub const STARTUP_BUDGET: Duration = Duration::from_secs(15);
/// Budget for the process to exit after a SHUTDOWN, generous for debug builds.
pub const EXIT_BUDGET: Duration = Duration::from_secs(8);

/// The API key the base env gives the extension.
pub const TEST_API_KEY: &str = "0000000000000000000000000000000a";

const BINARY: &str = env!("CARGO_BIN_EXE_bottlecap");

pub struct BottlecapBuilder {
    env: BTreeMap<String, String>,
    task_root: tempfile::TempDir,
}

impl BottlecapBuilder {
    /// Base env for a process that talks to `api`. Every intake URL points at
    /// `api` too, until [`intake`](Self::intake) replaces them.
    pub fn new(api: &FakeExtensionsApi) -> Self {
        let task_root = tempfile::tempdir().expect("tempdir");
        let mut env = BTreeMap::new();
        let set = |env: &mut BTreeMap<String, String>, k: &str, v: &str| {
            env.insert(k.to_string(), v.to_string());
        };
        set(&mut env, "AWS_LAMBDA_RUNTIME_API", &api.runtime_api());
        set(&mut env, "AWS_LAMBDA_FUNCTION_NAME", "test");
        set(&mut env, "AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "128");
        set(&mut env, "AWS_LAMBDA_FUNCTION_VERSION", "$LATEST");
        set(&mut env, "AWS_REGION", "us-east-1");
        set(&mut env, "AWS_DEFAULT_REGION", "us-east-1");
        set(
            &mut env,
            "LAMBDA_TASK_ROOT",
            &task_root.path().display().to_string(),
        );
        set(&mut env, "DD_API_KEY", TEST_API_KEY);
        set(&mut env, "DD_SITE", "datadoghq.com");
        set(&mut env, "DD_LOG_LEVEL", "debug");
        set(&mut env, "DD_SERVERLESS_FLUSH_STRATEGY", "end");
        // The fakes speak HTTP/1.1; the default client uses h2 prior knowledge.
        set(&mut env, "DD_HTTP_PROTOCOL", "http1");
        // Trace stats follow DD_SITE over https. Route https through a closed
        // port so they fail at once instead of reaching the network; the http
        // intake URLs bypass an https-only proxy.
        set(&mut env, "DD_PROXY_HTTPS", CLOSED_PORT_URL);
        let mut builder = Self { env, task_root };
        builder.set_intake(&api.base_url());
        builder
    }

    fn set_intake(&mut self, url: &str) {
        for key in [
            "DD_DD_URL",
            "DD_URL",
            "DD_APM_DD_URL",
            "DD_LOGS_CONFIG_LOGS_DD_URL",
        ] {
            self.env.insert(key.to_string(), url.to_string());
        }
    }

    /// Point every Datadog intake URL at `url` (`http://host:port`).
    pub fn intake(mut self, url: &str) -> Self {
        self.set_intake(url);
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    pub fn envs<'a>(mut self, pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        for (k, v) in pairs {
            self.env.insert(k.to_string(), v.to_string());
        }
        self
    }

    pub fn remove_env(mut self, key: &str) -> Self {
        self.env.remove(key);
        self
    }

    /// Lambda Managed Instances mode.
    pub fn managed_instance(self) -> Self {
        self.env("AWS_LAMBDA_INITIALIZATION_TYPE", "lambda-managed-instances")
    }

    pub fn flush_strategy(self, strategy: &str) -> Self {
        self.env("DD_SERVERLESS_FLUSH_STRATEGY", strategy)
    }

    /// Fetch the API key from Secrets Manager with container credentials
    /// served by `credentials_url`. The env carries no static AWS keys, so
    /// the extension must call that URL first.
    pub fn secret_from(self, credentials_url: &str) -> Self {
        self.remove_env("DD_API_KEY")
            .env(
                "DD_API_KEY_SECRET_ARN",
                "arn:aws:secretsmanager:us-east-1:123456789012:secret:dd-api-key",
            )
            .env("AWS_CONTAINER_CREDENTIALS_FULL_URI", credentials_url)
            .env("AWS_CONTAINER_AUTHORIZATION_TOKEN", "test-token")
    }

    pub fn spawn(self) -> BottlecapProcess {
        let stdout_path = self.task_root.path().join("bottlecap.stdout.log");
        let stderr_path = self.task_root.path().join("bottlecap.stderr.log");
        let stdout = std::fs::File::create(&stdout_path).expect("create stdout log");
        let stderr = std::fs::File::create(&stderr_path).expect("create stderr log");
        let mut cmd = Command::new(BINARY);
        cmd.env_clear();
        // Keep PATH so `ps` and friends resolve for the child's own tooling.
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn bottlecap");
        BottlecapProcess {
            pid: child.id(),
            child,
            started: Instant::now(),
            stdout_path,
            stderr_path,
            _task_root: self.task_root,
        }
    }
}

pub struct BottlecapProcess {
    pub pid: u32,
    child: Child,
    pub started: Instant,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    _task_root: tempfile::TempDir,
}

impl BottlecapProcess {
    pub fn builder(api: &FakeExtensionsApi) -> BottlecapBuilder {
        BottlecapBuilder::new(api)
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// Exit status if the process already exited.
    pub fn try_exit(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    pub fn is_running(&mut self) -> bool {
        self.try_exit().is_none()
    }

    /// Poll until the process exits or `budget` passes. On timeout the
    /// process is killed and `None` is returned.
    pub async fn wait_for_exit(&mut self, budget: Duration) -> Option<ExitStatus> {
        let start = Instant::now();
        loop {
            if let Some(status) = self.try_exit() {
                return Some(status);
            }
            if start.elapsed() > budget {
                self.kill();
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn stdout(&self) -> String {
        std::fs::read_to_string(&self.stdout_path).unwrap_or_default()
    }

    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    /// The binary under test and its build time, for assertion messages.
    pub fn binary_info() -> String {
        let mtime = std::fs::metadata(BINARY)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or_else(
                || "unknown".to_string(),
                |d| {
                    format!(
                        "{}s ago",
                        SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map(|now| now.as_secs().saturating_sub(d.as_secs()))
                            .unwrap_or_default()
                    )
                },
            );
        format!("binary {BINARY} (built {mtime})")
    }

    /// Last `n` lines of stdout and stderr, for assertion messages. Drops the
    /// per-tick `/proc` noise a macOS host produces.
    pub fn log_tail(&self, n: usize) -> String {
        let out = self.stdout();
        let err = self.stderr();
        let tail = |text: &str| {
            let lines: Vec<&str> = text
                .lines()
                .filter(|l| !l.contains("Could not list /proc files"))
                .collect();
            let start = lines.len().saturating_sub(n);
            lines[start..].join("\n")
        };
        format!(
            "{}\n--- stdout tail ---\n{}\n--- stderr tail ---\n{}",
            Self::binary_info(),
            tail(&out),
            tail(&err)
        )
    }

    pub fn log_contains(&self, needle: &str) -> bool {
        self.stdout().contains(needle) || self.stderr().contains(needle)
    }

    /// Lines of stderr that report a Rust panic. Dev builds unwind and keep
    /// running; release builds abort. Both print `panicked at`.
    pub fn panic_lines(&self) -> Vec<String> {
        self.stderr()
            .lines()
            .filter(|l| l.contains("panicked at"))
            .map(str::to_string)
            .collect()
    }

    pub fn assert_no_panic(&self) {
        let panics = self.panic_lines();
        assert!(
            panics.is_empty(),
            "bottlecap panicked:\n{}\n{}",
            panics.join("\n"),
            self.log_tail(40)
        );
    }

    // --- resource usage -------------------------------------------------------

    /// Resident set size in KiB, if the process is alive.
    pub fn rss_kb(&self) -> Option<u64> {
        proc_status_kb(self.pid, "VmRSS:").or_else(|| ps_kb(self.pid, "rss="))
    }

    /// Peak resident set size in KiB (Linux only).
    pub fn hwm_kb(&self) -> Option<u64> {
        proc_status_kb(self.pid, "VmHWM:")
    }

    /// CPU time (user + system) in milliseconds, if the process is alive.
    pub fn cpu_ms(&self) -> Option<u64> {
        proc_stat_cpu_ms(self.pid).or_else(|| ps_cpu_ms(self.pid))
    }

    /// Whether RSS growth checks are meaningful on this platform.
    pub fn can_measure_memory() -> bool {
        cfg!(target_os = "linux")
    }
}

impl Drop for BottlecapProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

fn proc_status_kb(pid: u32, key: &str) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find(|l| l.starts_with(key))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn proc_stat_cpu_ms(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit(')').next()?;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // Fields after `)`: state(0) ... utime(11) stime(12).
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    // CLK_TCK is 100 on every Linux we run on (Lambda included); a test that
    // needs the exact value should read it through libc::sysconf.
    let ticks_per_sec = 100;
    Some((utime + stime) * 1000 / ticks_per_sec)
}

fn ps_field(pid: u32, field: &str) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", field, "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn ps_kb(pid: u32, field: &str) -> Option<u64> {
    ps_field(pid, field)?.parse().ok()
}

/// `ps -o time=` prints `MM:SS.ss`, `HH:MM:SS` or `DD-HH:MM:SS`.
fn ps_cpu_ms(pid: u32) -> Option<u64> {
    let text = ps_field(pid, "time=")?;
    let (days, clock) = match text.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().ok()?, rest),
        None => (0.0, text.as_str()),
    };
    let mut total_ms: f64 = days * 24.0 * 3600.0 * 1000.0;
    let mut clock_ms = 0.0;
    for part in clock.split(':') {
        clock_ms = clock_ms * 60.0 + part.parse::<f64>().ok()? * 1000.0;
    }
    total_ms += clock_ms;
    Some(total_ms as u64)
}
