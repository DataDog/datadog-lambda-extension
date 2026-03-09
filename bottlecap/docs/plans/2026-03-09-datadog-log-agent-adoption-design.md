# datadog-log-agent Adoption Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to create the implementation plan from this design.

**Goal:** Replace bottlecap's `src/logs/` aggregator and flusher with the shared `datadog-log-agent` crate, while keeping all Lambda-specific logic (`LambdaProcessor`) in bottlecap.

**Architecture:** `datadog-log-agent` owns batching limits, HTTP flushing, zstd compression, retry logic, and OWP support. `LambdaProcessor` remains in bottlecap and continues to own Lambda On-Demand lifecycle concerns (orphan logs, request_id tracking, OOM detection, processing rules). `LambdaProcessor` produces `Vec<LogEntry>` instead of `Vec<String>`.

**Tech Stack:** Rust, tokio, reqwest, zstd, mockito (tests), datadog-log-agent (path dep → git dep post-merge)

---

## Scope

This design covers changes in **two repos**:

1. **`serverless-components`** — add `is_oom_message()` utility to `datadog-log-agent`
2. **`datadog-lambda-extension`** — adopt `datadog-log-agent` in bottlecap

---

## Changes in `serverless-components` (`datadog-log-agent`)

### New file: `crates/datadog-log-agent/src/lambda.rs`

Expose a pure OOM detection utility, extracted from bottlecap's `LambdaProcessor`:

```rust
/// Returns true if the message matches a known Lambda OOM error pattern.
pub fn is_oom_message(message: &str) -> bool {
    // The 7 Lambda OOM patterns (extracted from bottlecap lambda/processor.rs)
}
```

Re-export from `lib.rs`:
```rust
pub mod lambda;
pub use lambda::is_oom_message;
```

---

## Changes in `datadog-lambda-extension` (bottlecap)

### Dependency

```toml
# bottlecap/Cargo.toml
datadog-log-agent = { path = "../../serverless-components/crates/datadog-log-agent" }
# Post-merge: git = "https://github.com/DataDog/serverless-components", rev = "<merged-sha>"
```

### Files deleted from `src/logs/`

| File | Reason |
|---|---|
| `aggregator.rs` | Replaced by `datadog-log-agent`'s internal `Aggregator` |
| `aggregator_service.rs` | Replaced by `AggregatorService` / `AggregatorHandle` |
| `flusher.rs` | Replaced by `LogFlusher` / `LogFlusherConfig` |
| `constants.rs` | Same constants live in `datadog-log-agent` |
| `lambda/mod.rs` | `IntakeLog` / `Message` / `Lambda` types replaced by `LogEntry` |

### Files modified in `src/logs/`

#### `lambda/processor.rs` — core change

`LambdaProcessor` produces `Vec<LogEntry>` instead of `Vec<String>`:

- `ready_logs: Vec<String>` → `ready_logs: Vec<LogEntry>`
- `get_intake_log()` returns `LogEntry` instead of serialized `String`
- Lambda context maps to `attributes["lambda"] = { "arn": "...", "request_id": "..." }`
- OOM check: `self.is_oom(&msg)` → `datadog_log_agent::is_oom_message(&msg)`
- All other Lambda-specific logic unchanged (orphan logs, request_id, processing rules, managed instance mode)

**Data shape mapping:**

```
IntakeLog (before)                    LogEntry (after)
─────────────────────────────────     ────────────────────────────────────
message.message       ──────────────► message
message.timestamp     ──────────────► timestamp
message.status        ──────────────► status: Some(...)
hostname              ──────────────► hostname: Some(...)
service               ──────────────► service: Some(...)
source ("ddsource")   ──────────────► ddsource: Some(...)
tags ("ddtags")       ──────────────► ddtags: Some(...)
message.lambda.arn    ──────────────► attributes["lambda"]["arn"]
message.lambda.req_id ──────────────► attributes["lambda"]["request_id"]
```

The JSON serialized form is wire-compatible: `LogEntry` flattens `attributes` at the top level via `#[serde(flatten)]`.

#### `agent.rs`

Update type imports:
- `logs::aggregator_service::AggregatorHandle` → `datadog_log_agent::AggregatorHandle`
- `logs::aggregator_service::AggregatorService` → `datadog_log_agent::AggregatorService`

#### `processor.rs`

Update type imports only (no logic changes).

#### `mod.rs`

Remove module declarations for deleted files.

### Wiring in `main.rs` / `start_logs_agent()`

Replace `LogsFlusher` with `LogFlusher`:

```rust
use datadog_log_agent::{AggregatorHandle, AggregatorService, LogFlusher, LogFlusherConfig};

fn start_logs_agent(...) -> (Sender<TelemetryEvent>, LogFlusher, CancellationToken, AggregatorHandle) {
    let (service, handle) = AggregatorService::new();
    tokio::spawn(service.run());

    let config = LogFlusherConfig { api_key, ..LogFlusherConfig::from_env() };
    let flusher = LogFlusher::new(config, http_client, handle.clone());

    // LogsAgent and LambdaProcessor wiring unchanged
    (tx, flusher, cancel_token, handle)
}
```

`FlushingService` calls `flusher.flush().await` — method name is identical, no change needed there.

---

## Error Handling

- If `DD_API_KEY` is absent, `start_logs_agent` returns early (same behavior as `serverless-compat`)
- Retry logic (3 attempts, 403 = stop) is now owned by `datadog-log-agent`
- OPW mode: if `DD_OBSERVABILITY_PIPELINES_WORKER_LOGS_ENABLED=true`, `LogFlusherConfig::from_env()` picks it up automatically

---

## Testing

- `datadog-log-agent`'s existing integration tests cover: batch limits, compression, retry, OPW mode, additional endpoints
- In bottlecap: update existing `src/logs/` unit tests to use `LogEntry` instead of `IntakeLog`/serialized strings
- Add a smoke test in bottlecap verifying `LambdaProcessor` produces correctly shaped `LogEntry` (lambda attrs present)
- OOM utility: unit tests in `datadog-log-agent/src/lambda.rs` (moved from bottlecap)

---

## What Does NOT Change

- `LambdaProcessor` core logic: orphan logs, request_id tracking, managed instance mode, processing rules
- Event bus wiring (OOM, platform events)
- `LogsAgent` receive loop
- `FlushingService` flush orchestration
- All environment variable names (`DD_API_KEY`, `DD_SITE`, `DD_LOGS_CONFIG_USE_COMPRESSION`, etc.)
- Wire format to Datadog (same JSON shape)
