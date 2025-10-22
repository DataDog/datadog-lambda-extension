# Bottlecap

## Developing on Apple Silicon
One time setup:
1. `rustup target add aarch64-unknown-linux-gnu`
2. `brew install zig`
3. `cargo install cargo-zigbuild`

Then: `./runBottlecap.sh`

## Developing using Codespaces
Step 1: Create a codespace (code > codespaces > create codespace on main)

![img](./codespace.png)

Step 2: Hack in the `bottlecap` folder

Step 3: Test your change running `./runBottlecap.sh`

![img](./runBottlecap.png)

## Flush Strategies

Bottlecap supports several flush strategies that control when and how observability data (metrics, logs, traces) is sent to Datadog. The strategy is configured via the `DD_SERVERLESS_FLUSH_STRATEGY` environment variable.

**Important**: Flush strategies behave differently depending on the Lambda execution mode:
- **Managed Instance**: Uses continuous background flushing (flush strategies are ignored)
- **On-Demand**: Uses configurable flush strategies

### Managed Instance Mode vs On-Demand Mode

#### Managed Instance Mode
- **Activation**: Automatically enabled when Lambda uses managed instances (concurrent execution environment)
- **Flush Behavior**:
  - A dedicated background task continuously flushes data at regular intervals (default: 60 seconds)
  - All flushes are **non-blocking** and run concurrently with invocation processing
  - Prevents resource buildup by skipping a flush cycle if the previous flush is still in progress
  - `DD_SERVERLESS_FLUSH_STRATEGY` is **ignored** in this mode
- **Shutdown Behavior**:
  - Background flusher waits for pending flushes to complete before shutdown
  - Final flush ensures all remaining data is sent before the execution environment terminates
- **Use case**: High-concurrency Lambda functions with multiple concurrent invocations
- **Key advantage**: Zero flush overhead per invocation - flushing happens independently in the background

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
| **Managed Instance** | *Always Continuous* | ❌ No | ❌ No | High-concurrency Lambda with concurrent invocations |
| **On-Demand** | Default | Initially yes, then no | ✅ Yes | General use - auto-optimizes |
| **On-Demand** | End | ✅ Yes | ❌ No | Minimal overhead, sporadic invocations |
| **On-Demand** | EndPeriodically | ✅ Yes | ❌ No | Long-running functions with progress visibility |
| **On-Demand** | Periodically | ✅ Yes | ❌ No | Predictable flush timing |
| **On-Demand** | Continuously | ❌ No | ❌ No | High-throughput, latency-sensitive |

### Implementation Details

#### Managed Instance Mode Implementation
Located in `bottlecap/src/bin/bottlecap/main.rs` (lines 581-828):
- **Background Flusher Task** (lines 611-653):
  - Spawns at startup and runs until shutdown
  - Uses `tokio::select!` to handle periodic flush ticks and shutdown signals
  - Calls `PendingFlushHandles::spawn_non_blocking_flushes()` for each flush cycle
  - Skips flush if previous flush handles are still pending
- **Non-Blocking Flush Spawning** (lines 140-200):
  - Spawns separate async tasks for logs, traces, metrics, stats, and proxy flushes
  - Each task runs independently without blocking the main event loop
  - Failed payloads are tracked for retry in `await_flush_handles()`
- **Shutdown Handling** (lines 655-728):
  - Separate task waits for SHUTDOWN event from Extensions API
  - Cancels background flusher and signals main event loop
  - Final flush ensures all data is sent before termination (lines 788-826)

#### On-Demand Mode Implementation
Located in `bottlecap/src/bin/bottlecap/main.rs` (lines 830-1072):
- **Flush Control**: `bottlecap/src/lifecycle/flush_control.rs`
  - Evaluates flush strategy and invocation history
  - Returns `FlushDecision` enum: `End`, `Periodic`, `Continuous`, or `Dont`
- **Event Loop**: Uses `FlushControl::evaluate_flush_decision()` to determine flush behavior
  - `FlushDecision::End`: Waits for `platform.runtimeDone`, then performs blocking flush
  - `FlushDecision::Periodic`: Performs blocking flush at configured interval
  - `FlushDecision::Continuous`: Spawns non-blocking flush tasks (similar to Managed Instance)
  - `FlushDecision::Dont`: Skips flushing for this cycle
- **Configuration**: `bottlecap/src/config/flush_strategy.rs`
  - Deserializes `DD_SERVERLESS_FLUSH_STRATEGY` environment variable
  - Supports formats: `"end"`, `"end,<ms>"`, `"periodically,<ms>"`, `"continuously,<ms>"`

### Key Architectural Differences

| Aspect | Managed Instance Mode | On-Demand Mode |
|--------|----------------------|----------------|
| **Event Source** | Telemetry API (platform events) | Extensions API `/next` endpoint |
| **Invocation Model** | Concurrent invocations | Sequential invocations |
| **Flush Trigger** | Background interval timer | Invocation lifecycle + interval |
| **Strategy Config** | Ignored (always continuous) | Configurable via env var |
| **Main Loop** | Event bus processing | `/next` + event bus processing |
| **Shutdown Detection** | Separate task monitors `/next` | Main loop receives from `/next` |
