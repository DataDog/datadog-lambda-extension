# RustRover Remote Debugging Setup for Datadog Lambda Extension

## Overview

This setup enables remote debugging of the Datadog Lambda Extension (bottlecap) running inside a Docker container that emulates the AWS Lambda environment.

## Architecture Diagram


```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          HOST MACHINE (macOS ARM64)                         │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                         RustRover IDE                                 │  │
│  │                                                                       │  │
│  │  • Source: bottlecap/src/bin/bottlecap/main.rs                        │  │
│  │  • Symbol file: .binaries/bottlecap-arm64 (with debug_info)           │  │
│  │  • Path mapping: /tmp/dd/bottlecap → local bottlecap/                 │  │
│  │  • Breakpoints set in source code                                     │  │
│  └────────────────┬──────────────────────────────────────────────────────┘  │
│                   │                                                         │
│                   │ GDB Remote Protocol (localhost:2345)                    │
│                   ▼                                                         │
│  ┌────────────────────────────────┐     ┌───────────────────────────────┐   │
│  │      Makefile Targets          │     │     Port Mappings             │   │
│  │                                │     │                               │   │
│  │  make build   → Build image    │     │  9000  → Lambda invocations   │   │
│  │  make start   → Start container│     │  2345  → GDB debugging        │   │
│  │  make attach  → Attach debugger│     │                               │   │
│  │  make invoke  → Trigger Lambda │     │                               │   │
│  │  make logs    → Tail logs      │     │                               │   │
│  │  make status  → Check status   │     │                               │   │
│  │  make shell   → Container shell│     │                               │   │
│  │  make stop    → Stop container │     │                               │   │
│  └────────────────────────────────┘     └───────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                     Port Mappings: │ 2345:2345, 9000:8080
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│               DOCKER CONTAINER: lambda (Linux ARM64)                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                      Lambda Runtime (RIE)                              │ │
│  │  • Listens on :8080 (exposed as host :9000)                            │ │
│  └──────────┬──────────────────────────────────────────────┬──────────────┘ │
│             │                                              │                │
│             │ Extensions API                               │ Handler API    │
│             ▼                                              ▼                │
│  ┌─────────────────────────────┐              ┌───────────────────────────┐ │
│  │  Datadog Extension          │              │  Lambda Function          │ │
│  │                             │              │  Handler API              │ │
│  │  Extensions API             │              │                           │ │
│  │                             │              └───────────────────────────┘ │
│  └──────────┬──────────────────┘                                            │
│             │                                                               │
│             │ Attached via ptrace                                           │
│             ▼                                                               │
│  ┌─────────────────────────────┐                                            │
│  │    gdbserver                │                                            │
│  │    0.0.0.0:2345             │                                            │
│  │                             │                                            │
│  │  Attached to datadog-agent  │                                            │
│  │  Awaits debug commands      │                                            │
│  └─────────────────────────────┘                                            │
└─────────────────────────────────────────────────────────────────────────────┘
```
## Prerequisites

1. **RustRover IDE** installed on your Mac
2. **Docker** running locally

## Makefile Targets

The Makefile provides convenient targets for managing the debug environment:

| Target | Description |
|--------|-------------|
| `make build` | Build the Lambda container image with debug symbols |
| `make start` | Start the container in background (detached mode) |
| `make start DEBUG_WAIT=15` | Start with 15-second debug wait on first invocation |
| `make attach` | Attach gdbserver to the running extension process |
| `make invoke` | Trigger a Lambda invocation via HTTP (30s timeout) |
| `make logs` | Tail container logs (Ctrl+C to exit) |
| `make status` | Check if container is running |
| `make shell` | Open a bash shell inside the container |
| `make stop` | Stop and remove the container |
| `make help` | Show all available targets |

## Quick Start Guide

### 1. Build the Container

```bash
cd datadog-lambda-extension/local_tests/bottlecap
make build
```

This builds the bottlecap binary with debug symbols and creates the Docker image with:
- GDB and gdbserver for debugging
- Lambda Runtime Interface Emulator (RIE)
- 15-minute Lambda timeout for comfortable debugging

### 2. Start the Container

```bash
make start
```

The container starts in background with:
- Port `9000` for Lambda invocations
- Port `2345` for GDB remote debugging
- Lambda timeout: 900 seconds (15 minutes)

**Optional: Start with debug wait at startup:**
```bash
make start DEBUG_WAIT=15
```
This will pause the extension for 15 seconds on first invocation, giving you time to attach the debugger before any code executes. See [DEBUG_GUIDE.md](DEBUG_GUIDE.md) for details.

Check status:
```bash
make status
```

View logs:
```bash
make logs
```

### 3. Trigger First Invocation

**IMPORTANT**: The Lambda extension only starts when invoked for the first time. You must trigger an invocation before attaching the debugger:

```bash
make invoke
```

Watch the logs to confirm the extension has started:
```
DD_EXTENSION | DEBUG | Starting Datadog Extension v88
DD_EXTENSION | DEBUG | Datadog Next-Gen Extension ready in 49ms
```

### 4. Attach the Debugger

**After the extension has started, in a separate terminal:**
```bash
make attach
```

This attaches `gdbserver` to the running `datadog-agent` process. You should see:
```
Attached; pid = <process_id>
Listening on port 2345
```

**Note**: If using `DEBUG_WAIT`, you need to:
1. Run `make invoke &` to start the extension (it will wait)
2. Quickly run `make attach` within the wait period
3. Then connect RustRover immediately

### 5. Configure RustRover

#### 5.1: Create Remote Debug Configuration

1. **Run** → **Edit Configurations** → **+** → **Remote GDB Server**
2. Configure:
   - **Name**: `Remote Debug Lambda Extension`
   - **GDB**: `/usr/bin/lldb` (or system default)
   - **'target remote' args**: `localhost:2345`
   - **Symbol file**:
     ```
     <PROJECT_ROOT>/.binaries/bottlecap-arm64
     ```
   - **Path mappings**:
     - **Local**: `<PROJECT_ROOT>/bottlecap`
     - **Remote**: `/tmp/dd/bottlecap`

3. Click **Apply** and **OK**

**Important Notes:**
- The symbol file path must point to `.binaries/bottlecap-arm64` (the Linux ARM64 binary with debug symbols)
- The path mapping translates embedded debug paths (`/tmp/dd/bottlecap`) to your local source directory
- The source code does NOT need to exist at `/tmp/dd/bottlecap` in the container - this path is only in debug metadata

### 6. Set Breakpoints and Debug

1. Open `bottlecap/src/bin/bottlecap/main.rs` in RustRover
2. Set breakpoints in your code
3. **Run** → **Debug 'Remote Debug Lambda Extension'**
4. Wait for "Connected" in the Debug panel
5. Trigger an invocation:
   ```bash
   make invoke
   ```

Your breakpoints should hit, allowing you to:
- Step through code
- Inspect variables
- View call stack
- Examine memory

### 7. Stop the Container

```bash
make stop
```

## Complete Debugging Workflow

```bash
# Terminal 1: Build and start
make build          # Build the image (first time or after code changes)
make start          # Start container in background
make status         # Verify it's running

# Terminal 2: Monitor logs
make logs           # Watch extension startup

# Terminal 1: Trigger first invocation to start extension
make invoke         # Wait for "Datadog Next-Gen Extension ready" in logs

# Terminal 2: Attach debugger AFTER extension has started
make attach         # Attach gdbserver (keep this running)
                    # Should show: "Attached; pid = XX, Listening on port 2345"

# RustRover: Set breakpoints and connect
# Run → Debug 'Remote Debug Lambda Extension'

# Terminal 3: Trigger more invocations
make invoke         # Trigger Lambda (breakpoints should hit)
make invoke         # You now have 15 minutes per invocation for debugging

# When done
make stop           # Stop and cleanup
```

### Alternative: Startup Debugging Workflow

For debugging extension initialization code:

```bash
# Terminal 1: Build and start with debug wait
make build
make start DEBUG_WAIT=15  # Extension will wait 15s on first invocation

# Terminal 2: Monitor logs
make logs

# Terminal 1: Trigger invocation (runs in background)
make invoke &       # Extension starts and waits 15 seconds

# Terminal 3: Quickly attach within 15 seconds!
make attach         # You have 15s to attach

# RustRover: Immediately connect debugger
# Run → Debug 'Remote Debug Lambda Extension'
# Set breakpoints in initialization code and continue execution
```

## Troubleshooting

### Issue: Lambda Timeout After 1 Second

**Error:** `Task timed out after 1.00 seconds`

**Cause:** This was the old default timeout. The Dockerfile now sets `AWS_LAMBDA_FUNCTION_TIMEOUT=900` (15 minutes) for comfortable debugging.

**Solution:**
```bash
make stop
make build  # Rebuild with new timeout
make start
make invoke # Should no longer timeout
```

### Issue: Cannot Attach - Extension Process Not Found

**Cause:** The extension only starts when Lambda is invoked for the first time.

**Solution:**
```bash
# First, trigger an invocation to start the extension
make invoke

# Wait for logs to show: "Datadog Next-Gen Extension ready"
make logs

# Now attach the debugger
make attach
```

### Issue: "no executable code is associated with this line"

**Cause:** Path mapping is incorrect or symbol file path is wrong.

**Solution:**
1. Verify symbol file in RustRover points to `.binaries/bottlecap-arm64`
2. Check path mappings:
   - Local: `<your-path>/bottlecap`
   - Remote: `/tmp/dd/bottlecap`
3. Rebuild the container: `make build`

### Issue: Cannot connect to remote debugger

**Solution:**
```bash
# Check if container is running
make status

# Verify gdbserver is listening
nc -zv localhost 2345
# Should show: Connection succeeded

# Check if gdbserver is attached
make shell
ps aux | grep gdbserver
```

### Issue: Breakpoints not hitting

**Checklist:**
- ✅ Container is running: `make status`
- ✅ Debugger attached: `make attach` in separate terminal
- ✅ RustRover connected: Debug panel shows "Connected"
- ✅ Breakpoint on executable line (not on comments/blank lines)
- ✅ Code matches binary (rebuild if you made changes)
- ✅ Path mappings are correct

### Issue: Extension not starting

**Solution:**
```bash
# Check logs
make logs

# Get shell and inspect
make shell
ps aux | grep datadog-agent
ls -la /opt/extensions/
```

### Issue: Container name conflict

**Error:** `The container name "/bottlecap-lambda" is already in use`

**Solution:**
```bash
make stop  # This will stop and remove the existing container
make start # Now start fresh
```

## Advanced Usage

### Manual GDB Commands

If you need to manually control gdbserver:

```bash
# Get container shell
make shell

# Find the extension PID
pidof datadog-agent

# Manually attach gdbserver
gdbserver --attach 0.0.0.0:2345 <PID>
```

### View Extension Logs Inside Container

```bash
make shell
```

### Rebuild After Code Changes

```bash
# Stop running container
make stop

# Rebuild with new code
make build

# Start fresh
make start
make attach
# Continue debugging in RustRover
```

## Path Mapping Explained

The binary was compiled in a Docker build container where source code was mounted at `/tmp/dd/bottlecap`. The debug symbols embedded in the binary reference these paths:

```
Debug Symbol Path              RustRover Mapping         Local File
─────────────────────          ─────────────────         ──────────────
/tmp/dd/bottlecap/       -->   bottlecap/          -->   /Users/.../bottlecap/
src/bin/bottlecap/             src/bin/bottlecap/        src/bin/bottlecap/
main.rs                        main.rs                   main.rs
```

When RustRover receives debug information saying "execution is at `/tmp/dd/bottlecap/src/bin/bottlecap/main.rs:310`", it uses the path mapping to display your local `bottlecap/src/bin/bottlecap/main.rs` at line 310.

## Architecture Notes

- **Container**: Runs AWS Lambda Runtime Interface Emulator (RIE) on Linux ARM64
- **RIE Binary**: Uses the RIE for gdbserver compatibility
- **Extension**: Installed as `/opt/extensions/datadog-agent` (the bottlecap binary)
- **Lambda Lifecycle**: Extension starts on-demand during first invocation, then persists
- **Lambda Timeout**: Set to 900 seconds (15 minutes) for debugging - you have plenty of time!
- **Debugging**: GDB Remote Serial Protocol over TCP port 2345
- **Source Matching**: Debug symbols contain source paths from build time (`/tmp/dd/bottlecap`)
- **No Source Needed in Container**: Source code only needs to exist on your local machine

## Additional Resources

- **[DEBUG_GUIDE.md](DEBUG_GUIDE.md)** - Comprehensive debugging guide with detailed scenarios
- [RustRover Remote Debugging Documentation](https://www.jetbrains.com/help/rust/remote-debugging.html)
- [GDB Remote Serial Protocol](https://sourceware.org/gdb/current/onlinedocs/gdb/Remote-Protocol.html)
- [AWS Lambda Runtime Interface Emulator](https://github.com/aws/aws-lambda-runtime-interface-emulator)
- [AWS Lambda Extensions API](https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html)