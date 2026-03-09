# datadog-log-agent Adoption Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace bottlecap's `src/logs/` aggregator and flusher with the shared `datadog-log-agent` crate, while keeping `LambdaProcessor` in bottlecap.

**Architecture:** Two-repo change. First extend `datadog-log-agent` with `is_oom_message()`. Then adopt it in bottlecap: `LambdaProcessor` produces `Vec<LogEntry>` instead of `Vec<String>`, the deleted aggregator/flusher files are replaced by the crate's equivalents, and `FlushingService` is simplified (internal retry in `datadog-log-agent` replaces the external `RequestBuilder` retry loop).

**Tech Stack:** Rust, tokio, `datadog-log-agent` (path dep), `mockito`

**⚠️ Wire format note:** The new serialized shape changes `message` from a nested JSON object to a flat string + top-level `lambda` attributes. This is the correct Datadog Logs intake format. The old format (nested `message` object) was a bottlecap-specific convention.

---

## Repo 1: `serverless-components`

Work on branch: `tianning.li/SVLS-8573-datadog-log-agent`

---

### Task 1: Add `is_oom_message()` utility to `datadog-log-agent`

**Files:**
- Create: `crates/datadog-log-agent/src/lambda.rs`
- Modify: `crates/datadog-log-agent/src/lib.rs`

**Step 1: Write the failing test**

In a new file `crates/datadog-log-agent/src/lambda.rs`:

```rust
// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// The 7 Lambda runtime OOM error patterns (extracted from bottlecap's LambdaProcessor).
const OOM_PATTERNS: [&str; 7] = [
    "fatal error: runtime: out of memory",       // Go
    "java.lang.OutOfMemoryError",                // Java
    "JavaScript heap out of memory",             // Node
    "Runtime exited with error: signal: killed", // Node
    "MemoryError",                               // Python
    "failed to allocate memory (NoMemoryError)", // Ruby
    "OutOfMemoryException",                      // .NET
];

/// Returns `true` if `message` contains a known Lambda OOM error pattern.
///
/// This is a pure pattern-match utility shared between consumers
/// (bottlecap's `LambdaProcessor` and `serverless-compat`'s Lambda handler).
/// The caller decides the side-channel action (e.g. emit metric, send event).
pub fn is_oom_message(message: &str) -> bool {
    OOM_PATTERNS.iter().any(|&p| message.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_oom() {
        assert!(is_oom_message("fatal error: runtime: out of memory"));
    }

    #[test]
    fn test_java_oom() {
        assert!(is_oom_message("caused by java.lang.OutOfMemoryError: GC overhead"));
    }

    #[test]
    fn test_node_heap_oom() {
        assert!(is_oom_message("FATAL ERROR: CALL_AND_RETRY_LAST Allocation failed - JavaScript heap out of memory"));
    }

    #[test]
    fn test_node_killed_oom() {
        assert!(is_oom_message("Runtime exited with error: signal: killed"));
    }

    #[test]
    fn test_python_oom() {
        assert!(is_oom_message("MemoryError"));
    }

    #[test]
    fn test_ruby_oom() {
        assert!(is_oom_message("failed to allocate memory (NoMemoryError)"));
    }

    #[test]
    fn test_dotnet_oom() {
        assert!(is_oom_message("System.OutOfMemoryException: Array dimensions exceeded supported range"));
    }

    #[test]
    fn test_non_oom_returns_false() {
        assert!(!is_oom_message("connection refused"));
        assert!(!is_oom_message("timeout"));
        assert!(!is_oom_message(""));
    }
}
```

**Step 2: Run tests to verify they fail**

```bash
cd /path/to/serverless-components
cargo test -p datadog-log-agent lambda -- --nocapture
```

Expected: FAIL with `error[E0425]: cannot find function 'is_oom_message'` (module doesn't exist yet).

**Step 3: Create `lambda.rs` with the implementation**

Write the full file content from Step 1 to `crates/datadog-log-agent/src/lambda.rs`.

**Step 4: Add `pub mod lambda;` to `lib.rs`**

In `crates/datadog-log-agent/src/lib.rs`, add after the existing `pub mod` declarations:

```rust
pub mod lambda;
```

Also add to the re-exports section:
```rust
pub use lambda::is_oom_message;
```

**Step 5: Run tests to verify they pass**

```bash
cargo test -p datadog-log-agent lambda
```

Expected: 9 tests pass.

**Step 6: Run full test suite**

```bash
cargo test -p datadog-log-agent
```

Expected: all tests pass, no warnings.

**Step 7: Commit**

```bash
git add crates/datadog-log-agent/src/lambda.rs crates/datadog-log-agent/src/lib.rs
git commit -m "feat(datadog-log-agent): add is_oom_message() Lambda OOM utility"
```

---

## Repo 2: `datadog-lambda-extension`

Work on branch: `tianning.li/SVLS-8573-datadog-log-agent-adoption`

---

### Task 2: Add `datadog-log-agent` dependency

**Files:**
- Modify: `bottlecap/Cargo.toml`

**Step 1: Add the path dependency**

In `bottlecap/Cargo.toml`, in the `[dependencies]` section, add after the `datadog-fips` entry:

```toml
datadog-log-agent = { path = "../../serverless-components/crates/datadog-log-agent" }
```

> Note: This is a local path dependency for development. When the `serverless-components` PR merges, this will be changed to:
> `datadog-log-agent = { git = "https://github.com/DataDog/serverless-components", rev = "<merged-sha>" }`

**Step 2: Verify compilation**

```bash
cd bottlecap
cargo check -p bottlecap 2>&1 | head -20
```

Expected: no errors related to the new dependency (other errors are expected since the integration isn't wired yet).

**Step 3: Commit**

```bash
git add bottlecap/Cargo.toml
git commit -m "chore(bottlecap): add datadog-log-agent path dependency"
```

---

### Task 3: Replace `LambdaProcessor` internals

This is the core change: `LambdaProcessor` produces `Vec<LogEntry>` instead of `Vec<String>`.

**Files:**
- Modify: `bottlecap/src/logs/lambda/processor.rs`

**Step 1: Write a test that exercises the new shape**

At the bottom of the test module in `processor.rs` (after line ~1800), add:

```rust
#[tokio::test]
async fn test_process_produces_log_entry_with_lambda_attrs() {
    use datadog_log_agent::{AggregatorHandle, AggregatorService, LogEntry};

    let (service, aggregator_handle) = AggregatorService::new();
    let _task = tokio::spawn(service.run());

    let tags = HashMap::from([("env".to_string(), "test".to_string())]);
    let config = Arc::new(config::Config {
        service: Some("my-fn".to_string()),
        tags: tags.clone(),
        ..config::Config::default()
    });
    let tags_provider = Arc::new(provider::Provider::new(
        Arc::clone(&config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([("function_arn".to_string(), "arn:aws:lambda:us-east-1:123:function:my-fn".to_string())]),
    ));
    let (tx, _) = tokio::sync::mpsc::channel(2);
    let mut processor = LambdaProcessor::new(tags_provider, config, tx, false);

    let event = TelemetryEvent {
        time: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        record: TelemetryRecord::Function(Value::String("hello from lambda".to_string())),
    };
    processor.process(event, &aggregator_handle).await;

    let batches = aggregator_handle.get_batches().await.unwrap();
    assert_eq!(batches.len(), 1);

    let entries: Vec<serde_json::Value> = serde_json::from_slice(&batches[0]).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["message"], "hello from lambda");
    assert_eq!(entries[0]["service"], "my-fn");
    assert_eq!(entries[0]["ddsource"], "lambda");
    // Lambda context in flat attributes
    assert!(entries[0]["lambda"]["arn"].is_string());
}
```

**Step 2: Run to verify it fails**

```bash
cargo test -p bottlecap test_process_produces_log_entry_with_lambda_attrs -- --nocapture
```

Expected: compile error — `AggregatorService` doesn't exist in this scope yet.

**Step 3: Implement the changes to `processor.rs`**

Make the following changes to `bottlecap/src/logs/lambda/processor.rs`:

**3a. Update imports** (top of file, replace existing log-related imports):

Replace:
```rust
use crate::logs::aggregator_service::AggregatorHandle;
use crate::logs::lambda::{IntakeLog, Message};
```

With:
```rust
use datadog_log_agent::{AggregatorHandle, LogEntry};
```

**3b. Update `LambdaProcessor` struct fields**:

Replace:
```rust
    orphan_logs: Vec<IntakeLog>,
    ready_logs: Vec<String>,
```

With:
```rust
    orphan_logs: Vec<LogEntry>,
    ready_logs: Vec<LogEntry>,
```

**3c. Update `impl Processor<IntakeLog>` to `impl Processor<LogEntry>`**:

Replace:
```rust
impl Processor<IntakeLog> for LambdaProcessor {}
```

With:
```rust
impl Processor<LogEntry> for LambdaProcessor {}
```

**3d. Replace `is_oom_error` call with `datadog_log_agent::is_oom_message`**

There are two occurrences (in `TelemetryRecord::Function` and `TelemetryRecord::Extension` match arms). In both places, replace:

```rust
if is_oom_error(&message) {
```

With:

```rust
if datadog_log_agent::is_oom_message(&message) {
```

Then delete the local `OOM_ERRORS` constant and `is_oom_error` function (lines 42-56).

**3e. Rewrite `get_intake_log` to return `LogEntry`**:

Replace the entire `fn get_intake_log` method (lines 357-431) with:

```rust
fn get_log_entry(&mut self, mut lambda_message: Message) -> Result<LogEntry, Box<dyn Error>> {
    // Assign request_id from message or context if available
    lambda_message.lambda.request_id = match lambda_message.lambda.request_id {
        Some(request_id) => Some(request_id),
        None => {
            if self.invocation_context.request_id.is_empty() || self.is_managed_instance_mode {
                None
            } else {
                Some(self.invocation_context.request_id.clone())
            }
        }
    };

    // Check if message is a JSON object that might have tags to extract
    let parsed_json = serde_json::from_str::<serde_json::Value>(lambda_message.message.as_str());

    let (final_message, tags, status) =
        if let Ok(serde_json::Value::Object(mut json_obj)) = parsed_json {
            let mut tags = self.tags.clone();
            let msg = Self::extract_tags_and_get_message(
                &mut json_obj,
                &mut tags,
                lambda_message.message.clone(),
            );

            // Extract log level from JSON (AWS JSON log format / Powertools).
            let status = json_obj
                .get("level")
                .or_else(|| json_obj.get("status"))
                .and_then(|v| v.as_str())
                .and_then(map_log_level_to_status)
                .map_or(lambda_message.status.clone(), std::string::ToString::to_string);

            (msg, tags, status)
        } else {
            (lambda_message.message, self.tags.clone(), lambda_message.status)
        };

    let mut attrs = serde_json::Map::new();
    attrs.insert(
        "lambda".to_string(),
        serde_json::json!({
            "arn": lambda_message.lambda.arn,
            "request_id": lambda_message.lambda.request_id,
        }),
    );

    let entry = LogEntry {
        message: final_message,
        timestamp: lambda_message.timestamp,
        hostname: Some(self.function_arn.clone()),
        service: Some(self.service.clone()),
        ddsource: Some(LAMBDA_RUNTIME_SLUG.to_string()),
        ddtags: Some(tags),
        status: Some(status),
        attributes: attrs,
    };

    let has_request_id = entry
        .attributes
        .get("lambda")
        .and_then(|l| l.get("request_id"))
        .map_or(false, |v| !v.is_null());

    if has_request_id || self.is_managed_instance_mode {
        Ok(entry)
    } else {
        self.orphan_logs.push(entry);
        Err("No request_id available, queueing for later".into())
    }
}
```

**3f. Update `make_log` to call `get_log_entry`**:

Replace:
```rust
async fn make_log(&mut self, event: TelemetryEvent) -> Result<IntakeLog, Box<dyn Error>> {
    match self.get_message(event).await {
        Ok(lambda_message) => self.get_intake_log(lambda_message),
        Err(e) => Err(e),
    }
}
```

With:
```rust
async fn make_log(&mut self, event: TelemetryEvent) -> Result<LogEntry, Box<dyn Error>> {
    match self.get_message(event).await {
        Ok(lambda_message) => self.get_log_entry(lambda_message),
        Err(e) => Err(e),
    }
}
```

**3g. Update `process_and_queue_log`**:

Replace:
```rust
fn process_and_queue_log(&mut self, mut log: IntakeLog) {
    let should_send_log = self.logs_enabled
        && LambdaProcessor::apply_rules(&self.rules, &mut log.message.message);
    if should_send_log && let Ok(serialized_log) = serde_json::to_string(&log) {
        drop(log);
        self.ready_logs.push(serialized_log);
    }
}
```

With:
```rust
fn process_and_queue_log(&mut self, mut log: LogEntry) {
    let should_send_log = self.logs_enabled
        && LambdaProcessor::apply_rules(&self.rules, &mut log.message);
    if should_send_log {
        self.ready_logs.push(log);
    }
}
```

**3h. Update the orphan log patch in `process`**:

The current code at lines 486-492:
```rust
let orphan_logs = std::mem::take(&mut self.orphan_logs);
for mut orphan_log in orphan_logs {
    orphan_log.message.lambda.request_id =
        Some(self.invocation_context.request_id.clone());
    self.process_and_queue_log(orphan_log);
}
```

Replace with:
```rust
let orphan_logs = std::mem::take(&mut self.orphan_logs);
for mut orphan_log in orphan_logs {
    // Patch request_id in the lambda attribute map
    if let Some(lambda_val) = orphan_log.attributes.get_mut("lambda") {
        if let Some(obj) = lambda_val.as_object_mut() {
            obj.insert(
                "request_id".to_string(),
                serde_json::Value::String(self.invocation_context.request_id.clone()),
            );
        }
    }
    self.process_and_queue_log(orphan_log);
}
```

**3i. Update `insert_batch` call in `process`**:

The current code at line 495-499:
```rust
if !self.ready_logs.is_empty()
    && let Err(e) = aggregator_handle.insert_batch(std::mem::take(&mut self.ready_logs))
{
    debug!("Failed to send logs to aggregator: {}", e);
}
```

No change needed — the call signature is identical: `insert_batch(Vec<LogEntry>)`. ✓

**3j. Update tests in `processor.rs`**

In the test module, the `AggregatorService` import changes:

Replace:
```rust
use crate::logs::aggregator_service::AggregatorService;
```

With:
```rust
use datadog_log_agent::{AggregatorHandle, AggregatorService};
```

Anywhere `AggregatorService::default()` is called, change to `AggregatorService::new()`.

Also, `Lambda` type is no longer imported from `crate::logs::lambda`. Remove that import. Any test that constructs a `Lambda` struct directly needs to be updated (orphan log tests use `IntakeLog` which no longer exists — those tests will need to be rewritten to check the aggregator's output instead).

**Step 4: Run tests**

```bash
cargo test -p bottlecap -- logs::lambda::processor 2>&1 | tail -20
```

Expected: all existing processor tests pass, plus the new `test_process_produces_log_entry_with_lambda_attrs` test.

**Step 5: Commit**

```bash
git add bottlecap/src/logs/lambda/processor.rs
git commit -m "feat(bottlecap): LambdaProcessor produces LogEntry via datadog-log-agent"
```

---

### Task 4: Delete obsolete `src/logs/` files and fix imports

**Files:**
- Delete: `bottlecap/src/logs/aggregator.rs`
- Delete: `bottlecap/src/logs/aggregator_service.rs`
- Delete: `bottlecap/src/logs/constants.rs`
- Delete: `bottlecap/src/logs/flusher.rs`
- Delete: `bottlecap/src/logs/lambda/mod.rs`
- Modify: `bottlecap/src/logs/mod.rs`
- Modify: `bottlecap/src/logs/agent.rs`
- Modify: `bottlecap/src/logs/processor.rs`

**Step 1: Delete the five files**

```bash
rm bottlecap/src/logs/aggregator.rs
rm bottlecap/src/logs/aggregator_service.rs
rm bottlecap/src/logs/constants.rs
rm bottlecap/src/logs/flusher.rs
rm bottlecap/src/logs/lambda/mod.rs
```

**Step 2: Update `bottlecap/src/logs/mod.rs`**

Replace the full content with:

```rust
pub mod agent;
pub mod lambda;
pub mod processor;
```

(Removed: `aggregator`, `aggregator_service`, `constants`, `flusher`. The `lambda` module now only has `processor.rs`.)

**Step 3: Update `bottlecap/src/logs/agent.rs`**

Replace:
```rust
use crate::logs::{aggregator_service::AggregatorHandle, processor::LogsProcessor};
```

With:
```rust
use crate::logs::processor::LogsProcessor;
use datadog_log_agent::AggregatorHandle;
```

**Step 4: Update `bottlecap/src/logs/processor.rs`**

Replace:
```rust
use crate::logs::aggregator_service::AggregatorHandle;
```

With:
```rust
use datadog_log_agent::AggregatorHandle;
```

**Step 5: Verify compilation**

```bash
cargo check -p bottlecap 2>&1 | grep "^error" | head -20
```

Expected: only errors in `flushing/service.rs` and `main.rs` about `LogsFlusher` (which we haven't updated yet). No errors in `src/logs/`.

**Step 6: Commit**

```bash
git add -u bottlecap/src/logs/
git commit -m "chore(bottlecap): delete aggregator/flusher/constants, wire AggregatorHandle from datadog-log-agent"
```

---

### Task 5: Update `FlushingService` and `main.rs` to use `LogFlusher`

**Files:**
- Modify: `bottlecap/src/flushing/handles.rs`
- Modify: `bottlecap/src/flushing/service.rs`
- Modify: `bottlecap/src/bin/bottlecap/main.rs`

**Step 1: Update `flushing/handles.rs`**

Change `log_flush_handles` type from `Vec<JoinHandle<Vec<reqwest::RequestBuilder>>>` to `Vec<JoinHandle<bool>>`:

Replace:
```rust
    /// Handles for log flush operations. Returns failed request builders for retry.
    pub log_flush_handles: Vec<JoinHandle<Vec<reqwest::RequestBuilder>>>,
```

With:
```rust
    /// Handles for log flush operations. Returns true on success, false on failure.
    pub log_flush_handles: Vec<JoinHandle<bool>>,
```

**Step 2: Update `flushing/service.rs`**

**2a. Update imports** — replace:
```rust
use crate::logs::flusher::LogsFlusher;
```
With:
```rust
use datadog_log_agent::LogFlusher;
```

**2b. Update `FlushingService` struct field**:

Replace:
```rust
    logs_flusher: LogsFlusher,
```

With:
```rust
    logs_flusher: LogFlusher,
```

**2c. Update `FlushingService::new` signature**:

Replace:
```rust
    pub fn new(
        logs_flusher: LogsFlusher,
```

With:
```rust
    pub fn new(
        logs_flusher: LogFlusher,
```

**2d. Update `spawn_non_blocking` — log flush task**:

Replace:
```rust
        // Spawn logs flush
        let lf = self.logs_flusher.clone();
        self.handles
            .log_flush_handles
            .push(tokio::spawn(async move { lf.flush(None).await }));
```

With:
```rust
        // Spawn logs flush
        let lf = self.logs_flusher.clone();
        self.handles
            .log_flush_handles
            .push(tokio::spawn(async move { lf.flush().await }));
```

**2e. Update `await_handles` — log retry section**:

Replace the entire log handles section (lines 194-222):
```rust
        // Await log handles with retry
        for handle in self.handles.log_flush_handles.drain(..) {
            match handle.await {
                Ok(retry) => {
                    if !retry.is_empty() {
                        debug!(
                            "FLUSHING_SERVICE | redriving {:?} log payloads",
                            retry.len()
                        );
                    }
                    for item in retry {
                        let lf = self.logs_flusher.clone();
                        match item.try_clone() {
                            Some(item_clone) => {
                                joinset.spawn(async move {
                                    lf.flush(Some(item_clone)).await;
                                });
                            }
                            None => {
                                error!("FLUSHING_SERVICE | Can't clone redrive log payloads");
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("FLUSHING_SERVICE | redrive log error {e:?}");
                }
            }
        }
```

With:
```rust
        // Await log handles (retries handled internally by LogFlusher)
        for handle in self.handles.log_flush_handles.drain(..) {
            match handle.await {
                Ok(success) => {
                    if !success {
                        error!("FLUSHING_SERVICE | log flush failed after retries");
                        flush_error = true;
                    }
                }
                Err(e) => {
                    error!("FLUSHING_SERVICE | log flush task error {e:?}");
                    flush_error = true;
                }
            }
        }
```

**2f. Update `flush_blocking_inner` — log flush call**:

Replace:
```rust
        tokio::join!(
            self.logs_flusher.flush(None),
```

With:
```rust
        tokio::join!(
            self.logs_flusher.flush(),
```

**Step 3: Update `main.rs`**

**3a. Update imports** — in the `logs` import block:

Replace:
```rust
    logs::{
        agent::LogsAgent,
        aggregator_service::{
            AggregatorHandle as LogsAggregatorHandle, AggregatorService as LogsAggregatorService,
        },
        flusher::LogsFlusher,
    },
```

With:
```rust
    logs::agent::LogsAgent,
```

Add a new top-level import:
```rust
use datadog_log_agent::{
    AggregatorHandle as LogsAggregatorHandle, AggregatorService as LogsAggregatorService,
    FlusherMode, LogFlusher, LogFlusherConfig,
};
```

**3b. Rewrite `start_logs_agent`**:

The function needs to be `async` to call `api_key_factory.get_api_key().await`.

Replace the entire `fn start_logs_agent` (lines 1030-1072) with:

```rust
async fn start_logs_agent(
    config: &Arc<Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    tags_provider: &Arc<TagProvider>,
    event_bus: Sender<Event>,
    is_managed_instance_mode: bool,
    client: &Client,
) -> (
    Sender<TelemetryEvent>,
    LogFlusher,
    CancellationToken,
    LogsAggregatorHandle,
) {
    let api_key = api_key_factory
        .get_api_key()
        .await
        .unwrap_or_default();

    let (aggregator_service, aggregator_handle) = LogsAggregatorService::new();
    tokio::spawn(aggregator_service.run());

    let (mut agent, tx) = LogsAgent::new(
        Arc::clone(tags_provider),
        Arc::clone(config),
        event_bus,
        aggregator_handle.clone(),
        is_managed_instance_mode,
    );
    let cancel_token = agent.cancel_token();
    tokio::spawn(async move {
        agent.spin().await;
        debug!("LOGS_AGENT | Shutting down...");
        drop(agent);
    });

    let log_config = build_log_flusher_config(config, api_key);
    let flusher = LogFlusher::new(log_config, client.clone(), aggregator_handle.clone());

    (tx, flusher, cancel_token, aggregator_handle)
}

fn build_log_flusher_config(config: &Arc<Config>, api_key: String) -> LogFlusherConfig {
    let mode = if config.observability_pipelines_worker_logs_enabled {
        FlusherMode::ObservabilityPipelinesWorker {
            url: config.observability_pipelines_worker_logs_url.clone(),
        }
    } else {
        FlusherMode::Datadog
    };

    let additional_endpoints = config
        .logs_config_additional_endpoints
        .iter()
        .map(|e| format!("https://{}:{}", e.host, e.port))
        .collect();

    LogFlusherConfig {
        api_key,
        site: config.logs_config_logs_dd_url.clone(),
        mode,
        additional_endpoints,
        use_compression: config.logs_config_use_compression,
        compression_level: config.logs_config_compression_level,
        flush_timeout: std::time::Duration::from_secs(config.flush_timeout),
    }
}
```

> **Note:** `additional_endpoints` now uses the primary API key for all endpoints. Per-endpoint API keys from `logs_config_additional_endpoints.api_key` are not forwarded — this is a known limitation of the first adoption iteration.

**3c. Update the `start_logs_agent` call site in `extension_loop_active`**:

Since the function is now `async`, add `.await` at the call site. Change line ~301:

Replace:
```rust
    let (logs_agent_channel, logs_flusher, logs_agent_cancel_token, logs_aggregator_handle) =
        start_logs_agent(
            config,
            Arc::clone(&api_key_factory),
            &tags_provider,
            event_bus_tx.clone(),
            aws_config.is_managed_instance_mode(),
            &shared_client,
        );
```

With:
```rust
    let (logs_agent_channel, logs_flusher, logs_agent_cancel_token, logs_aggregator_handle) =
        start_logs_agent(
            config,
            Arc::clone(&api_key_factory),
            &tags_provider,
            event_bus_tx.clone(),
            aws_config.is_managed_instance_mode(),
            &shared_client,
        )
        .await;
```

**3d. Update `FlushingService::new` calls** — the type signature of `logs_flusher` changed from `LogsFlusher` to `LogFlusher`, but the variable name and usage is unchanged. No code changes needed at the two `FlushingService::new(logs_flusher_clone, ...)` call sites — Rust will infer the new type.

**Step 4: Run compilation check**

```bash
cargo check -p bottlecap 2>&1 | grep "^error"
```

Expected: no errors.

**Step 5: Run all bottlecap tests**

```bash
cargo test -p bottlecap 2>&1 | tail -20
```

Expected: all tests pass.

**Step 6: Commit**

```bash
git add bottlecap/src/flushing/handles.rs \
        bottlecap/src/flushing/service.rs \
        bottlecap/src/bin/bottlecap/main.rs
git commit -m "feat(bottlecap): adopt datadog-log-agent LogFlusher, simplify FlushingService log path"
```

---

### Task 6: Smoke test and verification

**Step 1: Build release binary**

```bash
cargo build -p bottlecap --release 2>&1 | tail -5
```

Expected: builds successfully.

**Step 2: Run full test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all tests pass.

**Step 3: Verify `src/logs/` shape**

```bash
ls bottlecap/src/logs/
```

Expected output:
```
agent.rs
lambda/
  processor.rs
mod.rs
processor.rs
```

The following files should NOT exist:
- `aggregator.rs` ✗
- `aggregator_service.rs` ✗
- `constants.rs` ✗
- `flusher.rs` ✗
- `lambda/mod.rs` ✗

**Step 4: Commit**

No code changes — this is verification only. If any fixes were needed, commit them here.

---

## Limitations Documented

- **Per-endpoint API keys**: `logs_config_additional_endpoints` API keys are not forwarded to `datadog-log-agent`. Additional endpoints use the primary API key. Track as follow-up.
- **Wire format change**: `message` is now a flat string (was a nested JSON object). `timestamp` and `status` are now top-level fields. This is the correct Datadog intake format.
- **API key refresh**: The API key is resolved once at startup via `ApiKeyFactory::get_api_key().await`. KMS/SSM rotations will not be picked up until the next extension restart. The old `Flusher` resolved lazily via `OnceCell` — same behavior in practice since the `OnceCell` resolved and cached on first flush.
