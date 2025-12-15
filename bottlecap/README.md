# Bottlecap

## Development
Use the `/scripts/build_bottlecap_layer.sh` and either publish it as a layer and test in Lambda or copy the binary into a container image and test there. Ask AJ or see the internal wiki for more.

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
