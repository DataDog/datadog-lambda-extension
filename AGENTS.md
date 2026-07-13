# AGENTS.md

This file provides guidance to coding agents when working with code in this repository.

## What This Project Is

The Datadog Lambda Extension ("Bottlecap") is an AWS Lambda Extension written in Rust that asynchronously collects metrics, traces, and logs from Lambda functions and forwards them to Datadog. It supports two operational modes: **On-Demand** (standard Lambda invocations) and **Managed Instance** (Lambda Managed Instances with concurrent invocations).

## Common Commands

All Rust code lives under `bottlecap/`. Run these from that directory unless noted.

## Code Style

- Do not be overly defensive and add unnecessary checks or tests unless you have a good reason.

## Pull Requests

- When creating a PR, follow the PR template at `.github/pull_request_template.md`.
- When making a change on a PR, update the PR title and summary if necessary.
- When reviewing a PR, make sure the PR summary accurately describes the code changes.

## Architecture

### Two Execution Modes

**On-Demand Mode** (standard Lambda): one concurrent execution per environment. Configurable flush strategies:
- `end` — flush only at end of invocation
- `end,<ms>` — flush at end + periodic interval
- `periodically,<ms>` — periodic blocking flushes
- `continuously,<ms>` — non-blocking background flushes
- `default` — adaptive (starts with `end`, switches to periodic after ~20 invocations)

**Managed Instance Mode** (detected via `AWS_LAMBDA_INITIALIZATION_TYPE`): multiple concurrent invocations; background continuous flushing always active.

### Core Data Flow

```
Event sources
  ├─ Lambda Extension API (INVOKE / SHUTDOWN polled via `extension::next_event`)
  ├─ Telemetry Listener   (logs from the Lambda telemetry API)
  ├─ Lifecycle Listener   (HTTP endpoints called by tracer libs: start/end invocation)
  ├─ DogStatsD Listener   (custom metrics)
  └─ Trace / OTLP agents  (traces from tracer libs)
        ↓
  Event Bus (central async distribution)
        ↓
  Aggregator Services (batch collection)
      LogsAggregatorService / TraceAggregatorService / MetricsAggregatorService
        ↓
  Flush Control (decides when to flush, per mode + strategy)
        ↓
  Flusher Services (send to Datadog intake)
      LogsFlusher / TraceFlusher / MetricsFlusher
```

### Key Source Locations (`bottlecap/src/`)

| Path | Purpose |
|------|---------|
| `bin/bottlecap/main.rs` | Entry point; initializes all subsystems for both modes |
| `config/` | Configuration parsing (flush strategy, AWS env, log level) |
| `event_bus/` | Central async event distribution |
| `lifecycle/` | Lambda invocation lifecycle events and flush control logic |
| `logs/` | Log aggregation and flushing |
| `traces/` | Trace processing, stats generation, flushing (most complex subsystem) |
| `metrics/` | Enhanced Lambda runtime metrics (lambda lifecycle, filesystem, usage). DogStatsD UDP server itself lives in the external `dogstatsd` crate |
| `otlp/` | OpenTelemetry Protocol (HTTP + gRPC) support |
| `proxy/` | HTTP proxy intercepting Lambda Runtime API requests (LWA, AppSec, trace propagation) |
| `appsec/` | Application Security Monitoring |
| `tags/` | Tag extraction and propagation |
| `fips/` | FIPS mode cryptography support |
