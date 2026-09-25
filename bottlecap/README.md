# Bottlecap

## Development
Use the `/scripts/build_bottlecap_layer.sh` and either publish it as a layer and test in Lambda or copy the binary into a container image and test there. Ask AJ or see the internal wiki for more.

## Local APM debugging with fake-intake

The `fake-intake` feature builds a standalone fake Datadog APM intake: a small HTTP server that accepts the same APM endpoints the extension flushes to, decodes the payloads, and reports them locally. It is gated behind a feature and is never part of `default` or `fips` production builds.

```bash
cd bottlecap
cargo build --bin fake-intake --features fake-intake
```

The binary wraps the shared `datadog-mock-intake` crate from the `serverless-components` repository. The same crate backs the `apm_integration_test` and `dsm_integration_test` integration tests, which run in every test build via a dev-dependency.

### Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FAKE_INTAKE_PORT` | `8127` | Loopback listener port; `0` picks a free port (the bound address is printed at startup) |
| `FAKE_INTAKE_FAIL_STATS_FIRST_N` | `0` | Return HTTP 500 for the first N stats request attempts |
| `FAKE_INTAKE_DUMP_DIR` | unset | Write one JSON envelope per successfully decoded request into this directory |

Malformed configuration values cause a clear error and a nonzero exit. Bind failures and dump-directory creation failures are also reported instead of being ignored.

### Endpoints and response codes

- `POST /api/v0.2/stats`: MessagePack `StatsPayload` (gzip, zstd, and identity accepted): `202 Accepted` when stored
- `POST /api/v0.2/traces`: protobuf `AgentPayload` (gzip, zstd, and identity accepted): `202 Accepted` when stored
- `POST /api/v0.1/pipeline_stats`: MessagePack DSM pipeline stats: `202 Accepted` when stored
- Malformed payloads return `400 Bad Request` (unless inside the stats failure-injection window, which returns `500`)
- Request bodies above the 2 MiB wire-body limit return `413 Payload Too Large` without being decoded

### Failure injection semantics

With `FAKE_INTAKE_FAIL_STATS_FIRST_N=N`, the first N stats *attempts* return 500 and their payloads are not captured by the intake. Attempts are counted per server instance, atomically, so concurrent requests cannot exceed the limit. Rejected attempts are still decoded, summarized, and dumped, so a rejected attempt and its successful retry are each visible with their own status. Trace and DSM requests do not consume the rejection budget.

### Request summaries and stats grouping

Each handled request produces one summary line on stderr (prefix `mock-intake:`) with the request identity, endpoint, content encoding (`identity` when absent), response status, and decoded payload count. For stats requests, hits are grouped by the full aggregation key: client service/env/version plus origin context (hostname, container, tags, git/image metadata, process tags, aggregation mode) and grouped dimensions (service, name, resource, type, DB type, HTTP status, gRPC status, synthetics, span kind, trace-root flag, HTTP method/endpoint, service source, peer tags, deprecated span-derived primary tags, and additional metric tags). Measurements (hits, errors, duration, sketches) and delivery metadata are excluded from the key, so matching dimensions combine across time buckets. Tag lists are sorted in the summary without modifying captured payloads.

### JSON dumps

With `FAKE_INTAKE_DUMP_DIR` set, each successfully decoded request attempt (including rejected stats attempts) writes one JSON file containing the request identity, endpoint, encoding, response status, and decoded payload. Trace payloads are serialized field by field because `AgentPayload` does not implement `Serialize` in `libdd-trace-protobuf` 4.0.1. DSM dumps contain only the fields the crate decodes. Filenames are collision-resistant and never overwrite earlier dumps.

### Smoke procedure

Point the test-mode trace processor (currently in the separate worktree / PR that adds the `bottlecap-test-mode` binary) at a local fake intake:

```bash
cd ../<test-mode-worktree>/bottlecap
cargo build --bin bottlecap-test-mode --features test-mode

cd ../<this-checkout>/bottlecap
cargo run --bin fake-intake --features fake-intake

# in another shell:
DD_APM_DD_URL=http://127.0.0.1:8127 \
DD_LAMBDA_EXTENSION_COMPUTE_STATS=true \
DD_SERVICE=fake-intake-smoke DD_ENV=local DD_VERSION=smoke \
./target/debug/bottlecap-test-mode
```

Unset `DD_SERVERLESS_FLUSH_STRATEGY` so flushing is manual, and make sure no ambient proxy intercepts localhost. Once `GET http://127.0.0.1:8126/info` succeeds, POST a MessagePack trace payload to `/v0.4/traces` (with `X-Datadog-Trace-Count`), then `POST /flush`. The `mock-intake:` summary lines should show the trace request and a stats request whose grouped hits match your spans, and the JSON dumps should contain both payloads.

## Flush Strategies

Bottlecap supports several flush strategies that control when and how observability data (metrics, logs, traces) is sent to Datadog. The strategy is configured via the `DD_SERVERLESS_FLUSH_STRATEGY` environment variable.

**Important**: Flush strategies behave differently depending on the Lambda execution mode:
- **Managed Instance**: Always uses continuous background flushing (only custom continuous intervals are respected)
- **On-Demand**: Uses configurable flush strategies

### Managed Instance Mode vs On-Demand Mode

#### Managed Instance Mode
Lambda Managed Instances run your functions on EC2 instances (managed by AWS) with multi-concurrent invocations. This requires setting up a **capacity provider** - a configuration that defines VPC settings, instance requirements, and scaling parameters for the managed instances.

- **Activation**: Detected automatically via the `AWS_LAMBDA_INITIALIZATION_TYPE` environment variable. When this equals `"lambda-managed-instances"`, Bottlecap enters Managed Instance mode
- **Flush Behavior**:
  - A dedicated background task continuously flushes data at regular intervals (default: 30 seconds)
  - All flushes are **non-blocking** and run concurrently with invocation processing
  - Prevents resource buildup by skipping a flush cycle if the previous flush is still in progress
  - Only `DD_SERVERLESS_FLUSH_STRATEGY=continuously,<ms>` is respected; all other strategies are overridden to continuous with default interval
- **Shutdown Behavior**:
  - Background flusher waits for pending flushes to complete before shutdown
  - Final flush ensures all remaining data is sent before the execution environment terminates
- **Execution Model**: Multi-concurrent invocations where one execution environment handles multiple invocations simultaneously (unlike traditional Lambda's one-invocation-per-environment model)
- **Use case**: Steady-state, high-volume workloads where optimizing costs with predictable capacity is desired
- **Key advantage**: Zero flush overhead per invocation - flushing happens independently in the background
- **Infrastructure**: Lambda launches 3 instances by default for availability zone resiliency when a function version is published to a capacity provider

#### On-Demand Mode (Traditional Mode)
- **Activation**: Default mode for standard Lambda execution (one invocation at a time)
- **Flush Behavior**:
  - Respects the configured `DD_SERVERLESS_FLUSH_STRATEGY`
  - Flush timing is tied to invocation lifecycle events
  - Can be blocking or non-blocking depending on the chosen strategy
- **Use case**: Standard Lambda functions with sequential invocation processing
- **Key advantage**: Fine-grained control over flush timing and behavior

### Available Strategies (On-Demand Mode Only)

#### `Default` (Recommended)
- **Configuration**: Set automatically when no strategy is specified, or explicitly via `DD_SERVERLESS_FLUSH_STRATEGY=default`
- **Behavior**: Adaptive - changes based on invocation frequency
  - **Initial behavior** (first ~20 invocations): Flushes at end of each invocation (blocking)
  - **After 20 invocations**: Switches to non-blocking continuous flushes
- **Interval**: 60 seconds
- **Use case**: Recommended for most serverless workloads - automatically optimizes for your traffic pattern

#### `End`
- **Configuration**: `DD_SERVERLESS_FLUSH_STRATEGY=end`
- **Behavior**: Always flushes at the end of each invocation (blocking)
- **Interval**: 15 minutes (effectively disables periodic flushing)
- **Use case**: Minimize flushing overhead - only flush once per invocation when the invocation is complete

#### `EndPeriodically`
- **Configuration**: `DD_SERVERLESS_FLUSH_STRATEGY=end,<milliseconds>` (e.g., `end,1000`)
- **Behavior**: Flushes both at the end of invocation AND periodically during long-running invocations (blocking)
- **Interval**: User-specified (in milliseconds)
- **Use case**: Long-running Lambda functions where you want data visibility during execution, not just at the end

#### `Periodically`
- **Configuration**: `DD_SERVERLESS_FLUSH_STRATEGY=periodically,<milliseconds>` (e.g., `periodically,60000`)
- **Behavior**: Always flushes at the specified interval (blocking)
- **Interval**: User-specified (in milliseconds)
- **Use case**: Predictable periodic flushing when you want guaranteed flush timing

#### `Continuously`
- **Configuration**: `DD_SERVERLESS_FLUSH_STRATEGY=continuously,<milliseconds>` (e.g., `continuously,60000`)
- **Behavior**: Spawns non-blocking async flush tasks at the specified interval
- **Interval**: User-specified (in milliseconds)
- **Use case**: High-throughput scenarios where invocation latency is critical and you can't afford to wait for flushes

### Summary Table

| Mode | Strategy | Blocking? | Adapts? | Best For |
|------|----------|-----------|---------|----------|
| **Managed Instance** | *Always Continuous* | ❌ No | ❌ No | Steady-state high-volume workloads with multi-concurrent invocations |
| **On-Demand** | Default | Initially yes, then no | ✅ Yes | General use - auto-optimizes |
| **On-Demand** | End | ✅ Yes | ❌ No | Minimal overhead, sporadic invocations |
| **On-Demand** | EndPeriodically | ✅ Yes | ❌ No | Long-running functions with progress visibility |
| **On-Demand** | Periodically | ✅ Yes | ❌ No | Predictable flush timing |
| **On-Demand** | Continuously | ❌ No | ❌ No | High-throughput, latency-sensitive |

### Implementation Details

#### Managed Instance Mode Implementation
Located in `bottlecap/src/bin/bottlecap/main.rs`:
- **Mode Detection** (`bottlecap/src/config/aws.rs`):
  - Checks if `AWS_LAMBDA_INITIALIZATION_TYPE` environment variable equals `"lambda-managed-instances"`
- **Event Subscription** (`bottlecap/src/extension/mod.rs`):
  - Only subscribes to `SHUTDOWN` events (not `INVOKE` events)
  - On-Demand mode subscribes to both `INVOKE` and `SHUTDOWN` events
- **Flush Strategy Override**:
  - Function: `get_flush_strategy_for_mode()`
  - If user configures a non-continuous strategy, it's overridden to continuous with a warning
  - Uses `DEFAULT_CONTINUOUS_FLUSH_INTERVAL` (30 seconds) from `flush_control.rs`
- **Main Event Loop**:
  - Processes events from the event bus (telemetry events like `platform.start`, `platform.report`)
  - Does NOT call `/next` endpoint for each invocation (only for shutdown)
  - Uses `tokio::select!` with biased ordering to prioritize telemetry events over shutdown signals
- **Background Flusher Task**:
  - Spawns at startup and runs until shutdown
  - Uses `tokio::select!` to handle periodic flush ticks and shutdown signals
  - Calls `PendingFlushHandles::spawn_non_blocking_flushes()` for each flush cycle
  - Skips flush if previous flush handles are still pending
- **Non-Blocking Flush Spawning**:
  - Method: `PendingFlushHandles::spawn_non_blocking_flushes()`
  - Spawns separate async tasks for logs, traces, metrics, stats, and proxy flushes
  - Each task runs independently without blocking the main event loop
  - Failed payloads are tracked for retry in `await_flush_handles()`
- **Shutdown Handling**:
  - Separate task waits for SHUTDOWN event from Extensions API
  - Cancels background flusher and signals main event loop
- **Final Flush**:
  - Function: `blocking_flush_all()`
  - Ensures all remaining data is sent before termination
  - Uses blocking flush with `force_flush_trace_stats=true`

#### On-Demand Mode Implementation
Located in `bottlecap/src/bin/bottlecap/main.rs`:
- **Flush Control** (`bottlecap/src/lifecycle/flush_control.rs`):
  - Function: `evaluate_flush_decision()`
  - Evaluates flush strategy and invocation history
  - Returns `FlushDecision` enum: `End`, `Periodic`, `Continuous`, or `Dont`
  - Adaptive behavior: After ~20 invocations, Default strategy switches from End to Continuous
- **Event Loop**: Uses `FlushControl::evaluate_flush_decision()` to determine flush behavior
  - `FlushDecision::End`: Waits for `platform.runtimeDone`, then performs blocking flush
  - `FlushDecision::Periodic`: Performs blocking flush at configured interval
  - `FlushDecision::Continuous`: Spawns non-blocking flush tasks (similar to Managed Instance)
  - `FlushDecision::Dont`: Skips flushing for this cycle
- **Final Flush**:
  - Function: `blocking_flush_all()`
  - Blocking flush with `force_flush_trace_stats=true`
  - Ensures all remaining data is sent before shutdown
- **Configuration** (`bottlecap/src/config/flush_strategy.rs`):
  - Deserializes `DD_SERVERLESS_FLUSH_STRATEGY` environment variable
  - Supports formats: `"end"`, `"end,<ms>"`, `"periodically,<ms>"`, `"continuously,<ms>"`

### Key Architectural Differences

| Aspect | Managed Instance Mode | On-Demand Mode |
|--------|----------------------|----------------|
| **Event Source** | Telemetry API (platform events) | Extensions API `/next` endpoint |
| **Invocation Model** | Multi-concurrent (one environment handles multiple invocations) | Single-concurrent (one invocation per environment) |
| **Scaling** | Asynchronous, CPU-based scaling | Reactive scaling with cold starts |
| **Pricing** | EC2 instance-based | Per-request duration-based |
| **Flush Trigger** | Background interval timer | Invocation lifecycle + interval |
| **Strategy Config** | Always continuous (custom intervals respected) | Configurable via env var |
| **Main Loop** | Event bus processing | `/next` + event bus processing |
| **Shutdown Detection** | Separate task monitors `/next` | Main loop receives from `/next` |

## References

### AWS Lambda Managed Instances Documentation
- [Introducing AWS Lambda Managed Instances: Serverless simplicity with EC2 flexibility](https://aws.amazon.com/blogs/aws/introducing-aws-lambda-managed-instances-serverless-simplicity-with-ec2-flexibility/) - AWS Blog announcement
- [Lambda Managed Instances - AWS Lambda Developer Guide](https://docs.aws.amazon.com/lambda/latest/dg/lambda-managed-instances.html) - Official AWS documentation
