# Debugging Bottlecap Extension Guide

This guide explains how to debug the bottlecap Lambda extension running in a local Docker container using RustRover (or any GDB-compatible debugger).

## Overview

The bottlecap extension is a Rust-based AWS Lambda extension. Unlike traditional processes that start when the container starts, Lambda extensions only initialize when Lambda is invoked for the first time.

**Key Insight**: You must trigger at least one Lambda invocation (`make invoke`) before the extension process exists and can be debugged. The debugging sequence is: build → start → **invoke** → attach → debug.

This guide covers two debugging approaches:

1. **Post-Startup Debugging**: Attach after the extension has started (recommended for most cases)
2. **Startup Debugging**: Debug from extension initialization using a wait period

## Prerequisites

- Docker running on your machine
- RustRover or CLion IDE (or any GDB-compatible debugger)
- The bottlecap extension built with debug symbols (`DEBUG=1`)
- RIE binary: The setup uses a custom Lambda Runtime Interface Emulator.

## Understanding Lambda Extension Lifecycle

Lambda extensions are **on-demand processes** that behave differently from traditional Docker processes:

1. **Container starts** (`make start`): The Lambda RIE starts, but the extension does NOT run yet
2. **First invocation** (`make invoke`): Lambda initializes and starts the extension process
3. **Extension registers**: The extension registers with Lambda Runtime API and enters its event loop
4. **Ready for debugging**: Only now does the datadog-agent process exist and can be attached

This means:
- Running `docker ps` shows the container, but the extension process isn't visible yet
- `make attach` will fail if no invocation has occurred
- You must always invoke Lambda first to start the extension before debugging

## Method 1: Post-Startup Debugging (Recommended)

This approach lets you attach the debugger after the extension has started and is ready to process invocations.

### Step 1: Build and Start the Container

```bash
cd local_tests/bottlecap

# Build the extension with debug symbols
make build

# Start the container
make start
```

### Step 2: Trigger First Invocation

The extension only starts when Lambda is invoked for the first time:

```bash
# In one terminal, monitor logs
make logs

# In another terminal, trigger invocation
make invoke
```

Watch the logs for:
```
DD_EXTENSION | DEBUG | Starting Datadog Extension v88
DD_EXTENSION | DEBUG | Datadog Next-Gen Extension ready in 49ms
```

### Step 3: Attach the Debugger

Once the extension is running, attach gdbserver:

```bash
make attach
```

You should see:
```
Attached; pid = 12
Listening on port 2345
```

### Step 4: Connect RustRover

1. In RustRover, go to **Run → Edit Configurations**
2. Click **+** and select **Remote Debug**
3. Configure:
   - **Name**: `Bottlecap Remote Debug`
   - **Target**: `localhost:2345`
   - **Symbol file**: `<PROJECT_ROOT>/.binaries/bottlecap-arm64`
   - Leave **Sysroot** empty
4. Click **OK**

5. Set breakpoints in your code (e.g., in `main.rs` or event handlers)

6. Start debugging: **Run → Debug 'Bottlecap Remote Debug'**

### Step 5: Trigger More Invocations

With the debugger attached and breakpoints set, trigger Lambda invocations to hit your breakpoints:

```bash
# Trigger invocation
make invoke

# Or trigger multiple times
for i in {1..5}; do make invoke; sleep 1; done
```

## Method 2: Startup Debugging

This approach is useful when you need to debug the extension initialization code itself.

### Step 1: Start with Debug Wait

```bash
cd local_tests/bottlecap

# Build
make build

# Start with 15-second wait for debugger
make start DEBUG_WAIT=15
```

This will display:
```
Container 'bottlecap-lambda' started. Use 'make logs' to view logs.
Debug mode: Extension will wait 15 seconds for debugger on first invocation
```

### Step 2: Trigger Invocation to Start Extension

```bash
# In one terminal, watch logs
make logs

# In another terminal, trigger the first invocation
# This starts the extension which will wait for 15 seconds
make invoke &
```

Watch the logs for:
```
DD_EXTENSION | DEBUG | Starting Datadog Extension v88
DD_EXTENSION | DEBUG | DD_DEBUG_WAIT_FOR_ATTACH: Waiting 15 seconds for debugger to attach...
DD_EXTENSION | DEBUG | Connect your debugger to port 2345 now!
```

### Step 3: Quickly Attach and Connect

You have 15 seconds to attach gdbserver and connect your debugger:

```bash
# Attach gdbserver (in a new terminal)
make attach
```

Then immediately connect RustRover as described in Method 1, Step 4.

### Step 4: Debug Initialization

The extension is now paused during initialization. You can:
- Step through the initialization code
- Set breakpoints in event handlers
- Continue execution to let the extension finish starting

After the wait period expires, you'll see:
```
DD_EXTENSION | DEBUG | DD_DEBUG_WAIT_FOR_ATTACH: Continuing execution...
```

## Common Debugging Scenarios

### Debug a Specific Invocation Handler

1. Start container: `make start`
2. Trigger first invocation to start extension: `make invoke`
3. Attach debugger: `make attach`
4. Set breakpoint in the handler (e.g., `main.rs:1143` in `handle_next_invocation`)
5. Connect RustRover debugger
6. Trigger another invocation: `make invoke`

### Debug Telemetry Event Processing

1. Start container: `make start DEBUG_WAIT=15`
2. Trigger invocation to start extension: `make invoke &`
3. Quickly attach: `make attach`
4. Set breakpoints in `handle_event_bus_event` (main.rs:1020)
5. Connect debugger
6. Let execution continue - breakpoints will hit as telemetry events arrive

### Debug Metrics/Logs/Traces Flushing

1. Start container normally: `make start`
2. Trigger invocation: `make invoke`
3. Attach: `make attach`
4. Set breakpoints in flushing code (main.rs:925 `blocking_flush_all`)
5. Connect debugger
6. Trigger more invocations to hit flush logic

## Troubleshooting

### "the input device is not a TTY" Error

This happens when running `make attach` from a non-interactive shell. The command requires TTY for gdbserver interaction. Run from a normal terminal session.

### Extension Process Not Found

If `make attach` can't find the datadog-agent process:
- Ensure you triggered at least one invocation: `make invoke`
- Check logs: `make logs` - look for "Starting Datadog Extension"
- Verify container is running: `make status`

### Debugger Can't Connect to Port 2345

- Check the container exposes port 2345: `docker ps` should show `0.0.0.0:2345->2345/tcp`
- Verify gdbserver is listening: `make attach` should show "Listening on port 2345"
- Check firewall settings

### Extension Exits Immediately

Lambda RIE may be shutting down the environment. Check:
- Container logs: `make logs`
- Ensure you're using the correct RIE binary
- The container must use the RIE from `local_tests/rie/bottlecap/arm64/rie`

### Timeout During Startup Debug Wait

If 15 seconds isn't enough:
- Increase the wait time: `make start DEBUG_WAIT=30`
- Or skip startup debugging and use post-startup method

### Lambda Invocation Times Out After 1 Second

If you see "Task timed out after 1.00 seconds" when running `make invoke`:
- **Root cause**: The `AWS_LAMBDA_FUNCTION_TIMEOUT` environment variable in the Dockerfile was set to 1 second
- **Fix**: The Dockerfile has been updated to use 900 seconds (15 minutes) to allow comfortable debugging
- **Verify**: Check `Dockerfile` line 7 shows `AWS_LAMBDA_FUNCTION_TIMEOUT=900`
- **Why 15 minutes**: This is AWS Lambda's maximum timeout, giving you plenty of time to step through code, inspect variables, and debug complex logic without the invocation timing out

## Key Code Locations for Breakpoints

- **Extension main entry**: `bottlecap/src/bin/bottlecap/main.rs:251` (`main()`)
- **Extension registration**: `main.rs:270` (`extension::register`)
- **Event loop (OnDemand mode)**: `main.rs:697` (OnDemand loop)
- **Invocation handling**: `main.rs:1143` (`handle_next_invocation`)
- **Event processing**: `main.rs:1020` (`handle_event_bus_event`)
- **Flushing**: `main.rs:925` (`blocking_flush_all`)

## Additional Resources

- [GDB Remote Debugging](https://sourceware.org/gdb/current/onlinedocs/gdb/Remote-Debugging.html)
- [RustRover Remote Debug Guide](https://www.jetbrains.com/help/rust/remote-debug.html)
- [AWS Lambda Extensions API](https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html)

## Clean Up

When done debugging:

```bash
# Stop the container
make stop

# View help for other commands
make help
```