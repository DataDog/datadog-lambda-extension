# Datadog Lambda Extension - Managed Instance Mode Testing

## Overview

This setup enables local testing of the Datadog Lambda Extension running in **Managed Instance Mode** using the AWS Lambda Runtime Interface Emulator (RIE). Managed Instance mode is a specialized initialization mode triggered by setting `AWS_LAMBDA_INITIALIZATION_TYPE="lambda-managed-instances"`, which changes how the extension behaves during Lambda initialization.

## What is Managed Instance Mode?

Managed Instance mode is activated when the extension detects it's running in a specific AWS Lambda environment (EC2 capacity provider). In this mode:
- The extension initializes with different telemetry endpoints
- Uses managed-instance-specific API routes for telemetry reporting
- Generates managed-instance-specific statistics and metrics
- Operates with enhanced monitoring capabilities

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          HOST MACHINE (macOS ARM64)                         │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                         RustRover IDE (Optional)                      │  │
│  │                                                                       │  │
│  │  • Source: bottlecap/src/bin/bottlecap/main.rs                        │  │
│  │  • Symbol file: .binaries/bottlecap-arm64 (with debug_info)           │  │
│  │  • Path mapping: /tmp/dd/bottlecap → local bottlecap/                 │  │
│  │  • Remote debugging available on port 2345                            │  │
│  └────────────────┬──────────────────────────────────────────────────────┘  │
│                   │                                                         │
│                   │ GDB Remote Protocol (localhost:2345)                    │
│                   ▼                                                         │
│  ┌────────────────────────────────┐     ┌───────────────────────────────┐   │
│  │      Makefile Targets          │     │     Port Mappings             │   │
│  │                                │     │                               │   │
│  │  make build   → Debug build    │     │  9000  → Lambda invocations   │   │
│  │  make build-release → Release  │     │  2345  → GDB debugging        │   │
│  │  make start   → Start container│     │                               │   │
│  │  make invoke  → Trigger Lambda │     │                               │   │
│  │  make logs    → Tail logs      │     │                               │   │
│  │  make status  → Check status   │     │                               │   │
│  │  make attach  → Attach debugger│     │                               │   │
│  │  make shell   → Container shell│     │                               │   │
│  │  make stop    → Stop container │     │                               │   │
│  └────────────────────────────────┘     └───────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                     Port Mappings: │ 2345:2345, 9000:8080
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│          DOCKER CONTAINER: managed-instance-lambda (Linux ARM64)            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Environment Variables:                                                     │
│  • AWS_LAMBDA_INITIALIZATION_TYPE="lambda-managed-instances" (Managed)      │
│  • DD_LOG_LEVEL="debug"                                                     │
│  • DD_FLUSHING_STRATEGY="continuously,1"                                    │
│                                                                             │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │        Lambda Runtime (Custom RIE for Managed Instance)                │ │
│  │  • Listens on :8080 (exposed as host :9000)                            │ │
│  │  • Custom RIE binary: local_tests/rie/managed-instance/arm64/rie       │ │
│  └──────────┬──────────────────────────────────────────┬──────────────────┘ │
│             │                                          │                    │
│             │ Extensions API                           │ Handler API        │
│             ▼                                          ▼                    │
│  ┌─────────────────────────────┐         ┌────────────────────────────────┐ │
│  │  Datadog Extension          │         │  Lambda Function               │ │
│  │  (Managed Instance Mode)    │         │  (Python Handler)              │ │
│  │                             │         │                                │ │
│  │  • MI-specific init         │         │  • Simple Python handler       │ │
│  │  • Enhanced telemetry       │         │  • Returns 200 status          │ │
│  │  • Continuous flushing      │         │                                │ │
│  └──────────┬──────────────────┘         └────────────────────────────────┘ │
│             │                                                               │
│             │ Debugging (optional)                                          │
│             ▼                                                               │
│  ┌─────────────────────────────┐                                            │
│  │    gdbserver (optional)     │                                            │
│  │    0.0.0.0:2345             │                                            │
│  │                             │                                            │
│  │  Attached to datadog-agent  │                                            │
│  └─────────────────────────────┘                                            │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Prerequisites

1. **Docker** installed and running
2. **Datadog API Key** for sending telemetry
3. **RustRover IDE** (optional, only needed for debugging)
4. **Rust toolchain** (for building the extension)

## Makefile Targets

| Target | Description |
|--------|-------------|
| `make build` | Build bottlecap in debug mode with debug symbols |
| `make build-release` | Build bottlecap in optimized release mode |
| `make start` | Start the container with managed instance mode enabled |
| `make invoke` | Trigger a Lambda invocation via HTTP |
| `make logs` | Tail container logs (Ctrl+C to exit) |
| `make status` | Check if container is running |
| `make attach` | Attach gdbserver to the running extension (for debugging) |
| `make shell` | Open a bash shell inside the container |
| `make stop` | Stop and remove the container |
| `make help` | Show all available targets |

## Quick Start Guide

> **⚠️ IMPORTANT: Known Limitation with Supplied RIE Runtime**
>
> The custom RIE (Runtime Interface Emulator) binaries provided in `local_tests/rie/managed-instance/` have **not been updated** to support the latest managed instance mode schema version changes.
>
> **Before running `make build`, you MUST temporarily update the local codebase:**
>
> In `bottlecap/src/extension/telemetry/mod.rs` (lines 48-52), change:
> ```rust
> let schema_version = if managed_instance_mode {
>     MANAGED_INSTANCE_SCHEMA_VERSION  // This is the correct version
> } else {
>     ON_DEMAND_SCHEMA_VERSION
> };
> ```
>
> **To:**
> ```rust
> let schema_version = if managed_instance_mode {
>     ON_DEMAND_SCHEMA_VERSION  // Temporary workaround for outdated RIE
> } else {
>     ON_DEMAND_SCHEMA_VERSION
> };
> ```
>
> This workaround forces the extension to use the `ON_DEMAND_SCHEMA_VERSION` even in managed instance mode, which is compatible with the current RIE binaries. **Remember to revert this change before committing any code or deploying to production.**
>
> This limitation only affects local testing with RIE. In actual AWS Lambda Managed Instance environments, the correct `MANAGED_INSTANCE_SCHEMA_VERSION` should be used.

### 1. Build the Container

Build the extension and container image:

```bash
cd local_tests/managed-instance
make build
```

This compiles bottlecap with debug symbols (for easier troubleshooting) and packages it into a Docker image with:
- Custom RIE binary for managed instance mode
- GDB and gdbserver for optional debugging
- Development tooling (Rust, curl, git, etc.)

For production-like testing, use:
```bash
make build-release
```

### 2. Set Your Datadog API Key

Export your Datadog API key as an environment variable:

```bash
export DD_API_KEY="your_datadog_api_key_here"
```

**Important:** The API key is required for the extension to send telemetry to Datadog. Without it, the extension will start but won't be able to flush data.

### 3. Start the Container

```bash
make start
```

The container starts with:
- **Port 9000**: Lambda invocations endpoint
- **Port 2345**: GDB remote debugging (optional)
- **Managed Instance mode**: Enabled via `AWS_LAMBDA_INITIALIZATION_TYPE="lambda-managed-instances"`
- **CPU limit**: 1 CPU to simulate Lambda resource constraints
- **Continuous flushing**: Enabled for testing real-time telemetry

Check status:
```bash
make status
```

View logs:
```bash
make logs
```

Look for the initialization message:
```
DD_EXTENSION | DEBUG | Starting Datadog Extension v88
DD_EXTENSION | DEBUG | Managed Instance mode detected
DD_EXTENSION | DEBUG | Datadog Next-Gen Extension ready in XXms
```

### 4. Invoke the Lambda Function

Trigger a Lambda invocation:

```bash
make invoke
```

This sends a POST request to the RIE, which:
1. Initializes the extension (first invocation only)
2. Invokes the Python handler function
3. Triggers the extension to process and flush telemetry
4. Returns the function response

Expected response:
```json
{"statusCode": 200, "body": "Hello from Lambda!"}
```

### 5. Monitor Telemetry

Watch the logs to see managed instance mode in action:

```bash
make logs
```

Look for:
- Extension initialization logs
- Managed-instance-specific telemetry endpoints
- Metric and trace flushing
- Continuous flush strategy execution

### 6. Stop the Container

When done testing:

```bash
make stop
```

## Testing Workflow

### Basic Testing

```bash
# Terminal 1: Build and start
make build          # Build with debug symbols
make start          # Start in background

# Terminal 2: Monitor logs
make logs           # Watch extension behavior

# Terminal 1: Test invocations
make invoke         # First invocation (initializes extension)
make invoke         # Subsequent invocations
make invoke         # Test continuous flushing

# Check Datadog UI for:
# - Metrics from service: dd-<username>-test
# - Environment: dev
# - Managed-instance-specific statistics

# When done
make stop
```

### Debugging Workflow

For debugging the extension with RustRover (see [bottlecap debugging guide](../bottlecap/README.md)):

**Correct Debugging Sequence:**

```bash
# Step 1: Build with debug symbols
make build          # Must use debug build (not release)

# Step 2: Start the container
make start          # Container runs in background

# Step 3: Set breakpoints in code
# Open bottlecap/src/bin/bottlecap/main.rs in RustRover
# Click in the gutter next to line numbers to set breakpoints

# Step 4: Attach gdbserver (in a separate terminal)
make attach         # Attach gdbserver to the running extension process
                    # Should show: "Attached; pid = XX"
                    #              "Listening on port 2345"
                    # Keep this terminal running!

# Step 5: Start debugger in RustRover IDE
# Run → Debug 'Remote Debug Lambda Extension'
# Wait for "Connected" message in Debug panel

# Step 6: Invoke Lambda to hit breakpoints
make invoke         # Trigger Lambda invocation - breakpoints should hit!
```

**Important Notes:**
- Build with `make build` (NOT `make build-release`) to include debug symbols
- The extension process must be running before `make attach` will work
- If `make attach` fails with "process not found", the extension hasn't started yet - trigger an initial invocation first
- Keep the `make attach` terminal running throughout your debugging session
- Set breakpoints BEFORE starting the debugger in step 5

## Environment Variables

The container is configured with these environment variables:

| Variable | Value | Purpose |
|----------|-------|---------|
| `AWS_LAMBDA_INITIALIZATION_TYPE` | `lambda-managed-instances` | Activates managed instance mode |
| `DD_API_KEY` | From host env | Authentication for Datadog API |
| `DD_SITE` | `datadoghq.com` | Datadog site to send data to |
| `DD_SERVICE` | `dd-<username>-test` | Service name for telemetry |
| `DD_ENV` | `dev` | Environment tag for telemetry |
| `DD_LOG_LEVEL` | `debug` | Extension logging level |
| `DD_FLUSHING_STRATEGY` | `continuously,1` | Flush every 1 second |

You can modify these in the Makefile or override when starting the container.

## Container Configuration

### Resource Limits
- **CPU**: Limited to 1 CPU (`--cpus=1`) to simulate Lambda constraints
- **Memory**: Uses Docker default (can be limited with `--memory` flag)

### Security Settings
- **SYS_PTRACE**: Enabled for gdbserver attachment (debugging)
- **seccomp**: Unconfined for debugging tools

### Volume Mounts
- **entrypoint.sh**: Mounted from local directory for easy modification

## Custom RIE Binary

The managed instance setup uses a custom RIE binary located at `local_tests/rie/managed-instance/arm64/rie`. This custom RIE may include:
- Modified behavior for managed instance mode testing
- Additional logging or instrumentation
- Specialized telemetry endpoint handling

The custom RIE is copied into the container at build time and used instead of the standard AWS RIE.

## Troubleshooting

### Issue: Container won't start

**Solution:**
```bash
# Check for conflicting containers
make status

# Force cleanup and restart
make stop
make start
```

### Issue: Extension not in managed instance mode

**Error:** Extension starts but doesn't show managed-instance-specific behavior

**Solution:**
1. Verify environment variable is set:
```bash
docker exec managed-instance-lambda env | grep AWS_LAMBDA_INITIALIZATION_TYPE
# Should output: AWS_LAMBDA_INITIALIZATION_TYPE=lambda-managed-instances
```

2. Check logs for managed instance mode detection:
```bash
make logs | grep -i "managed instance"
```

### Issue: No telemetry in Datadog

**Cause:** Missing or invalid DD_API_KEY

**Solution:**
```bash
# Verify API key is set
echo $DD_API_KEY

# Set it if missing
export DD_API_KEY="your_api_key"

# Restart container
make stop
make start
```

### Issue: Continuous flushing not working

**Solution:**
Check the flushing strategy configuration:
```bash
docker exec managed-instance-lambda env | grep DD_FLUSHING_STRATEGY
```

Increase logging and watch for flush events:
```bash
make logs | grep -i flush
```

### Issue: Cannot attach debugger

**Error:** `make attach` fails to find process

**Cause:** Extension hasn't started yet

**Solution:**
```bash
# First, invoke Lambda to start extension
make invoke

# Wait for startup message in logs
make logs | grep "Extension ready"

# Then attach
make attach
```

### Issue: Permission denied errors

**Solution:**
Ensure Docker has proper permissions:
```bash
# On macOS, check Docker Desktop settings
# On Linux, ensure user is in docker group
sudo usermod -aG docker $USER
```

## Advanced Usage

### Custom Configuration

Edit the Makefile to customize environment variables:

```makefile
start:
	docker run -d --name $(CONTAINER_NAME) \
	-p 9000:8080 \
	-p 2345:2345 \
	-e AWS_LAMBDA_INITIALIZATION_TYPE="lambda-managed-instances" \
	-e DD_API_KEY="$(DD_API_KEY)" \
	-e DD_SITE="datadoghq.com" \
	-e DD_SERVICE="my-custom-service" \
	-e DD_LOG_LEVEL="trace" \  # More verbose logging
	-e DD_ENV="staging" \
	-e DD_FLUSHING_STRATEGY="end_of_invocation" \  # Different strategy
	$(IMAGE)
```

### Interactive Container Shell

Get a shell inside the running container:

```bash
make shell
```

Then you can:
```bash
# Check extension process
ps aux | grep datadog-agent

# View extension logs
cat /tmp/datadog-agent.log

# Test network connectivity
curl https://api.datadoghq.com/api/v1/validate

# Inspect RIE
ps aux | grep aws-lambda-rie
```

### Manual Invocation with Custom Payload

```bash
curl -XPOST "http://localhost:9000/2015-03-31/functions/function/invocations" \
  -d '{"test": "custom", "requestId": "12345"}' \
  --max-time 30
```

### Rebuild After Code Changes

```bash
# Stop running container
make stop

# Rebuild with latest code
make build

# Start fresh
make start
make invoke
```

## Differences from Bottlecap Setup

| Aspect | Bottlecap Setup | Managed Instance Setup |
|--------|----------------|------------------------|
| **Initialization Mode** | Standard Lambda | Managed Instance mode (`lambda-managed-instances`) |
| **RIE Binary** | Standard RIE | Custom managed instance RIE |
| **Telemetry Endpoints** | Standard API routes | Managed-instance-specific routes |
| **Primary Use Case** | General debugging | Managed instance mode testing |
| **Flushing Strategy** | Various strategies | Continuous flushing by default |
| **Container Name** | `bottlecap-lambda` | `managed-instance-lambda` |

## Key Files

| File | Purpose |
|------|---------|
| `Dockerfile` | Container definition with custom RIE and tools |
| `Makefile` | Build and test automation |
| `index.py` | Simple Python Lambda handler |
| `entrypoint.sh` | Container entrypoint with RIE initialization |
| `local_tests/rie/managed-instance/arm64/rie` | Custom RIE binary for managed instance mode |

## Managed Instance Mode Implementation

The managed instance mode is implemented in the bottlecap extension:

- **Detection**: bottlecap/src/extension/telemetry/mod.rs - Checks `AWS_LAMBDA_INITIALIZATION_TYPE`
- **Telemetry**: Uses managed-instance-specific API routes for stats submission
- **Stats Generation**: Generates managed-instance-specific metrics and telemetry

See the main [CLAUDE.md](../../CLAUDE.md) for more details on the managed instance mode implementation.

## Additional Resources

- **[Bottlecap Debugging Guide](../bottlecap/README.md)** - Detailed debugging setup with RustRover
- **[AWS Lambda Extensions API](https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html)**
- **[Datadog Lambda Extension Documentation](https://docs.datadoghq.com/serverless/libraries_integrations/extension/)**
- **[AWS Lambda Runtime Interface Emulator](https://github.com/aws/aws-lambda-runtime-interface-emulator)**

## Testing Checklist

When testing managed instance mode, verify:

- ✅ Extension starts with managed instance mode detected
- ✅ Telemetry uses managed-instance-specific endpoints
- ✅ Continuous flushing strategy is active
- ✅ Metrics appear in Datadog with correct service/env tags
- ✅ Extension handles multiple invocations correctly
- ✅ Resource constraints (CPU) don't cause issues
- ✅ Debug logging shows managed-instance-specific behavior