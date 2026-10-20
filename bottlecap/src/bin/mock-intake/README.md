# mock-intake

A standalone mock Datadog APM intake for local APM debugging: a small HTTP server that accepts the same APM endpoints the extension flushes to, decodes the payloads, and reports them locally.

Built behind the `mock-intake` feature, which is never part of `default` or `fips` production builds:

```bash
cd bottlecap
cargo build --bin mock-intake --features mock-intake
```

The binary wraps the shared `datadog-mock-intake` crate from the `serverless-components` repository. The same crate backs the `apm_integration_test` and `dsm_integration_test` integration tests, which run in every test build via a dev-dependency.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `MOCK_INTAKE_PORT` | `8127` | Loopback listener port; `0` picks a free port (the bound address is printed at startup) |
| `MOCK_INTAKE_FAIL_STATS_FIRST_N` | `0` | Return HTTP 500 for the first N stats request attempts |
| `MOCK_INTAKE_DUMP_DIR` | unset | Write one JSON envelope per successfully decoded request into this directory |

Malformed configuration values cause a clear error and a nonzero exit. Bind failures and dump-directory creation failures are also reported instead of being ignored.

## Endpoints and response codes

- `POST /api/v0.2/stats`: MessagePack `StatsPayload` (gzip, zstd, and identity accepted): `202 Accepted` when stored
- `POST /api/v0.2/traces`: protobuf `AgentPayload` (gzip, zstd, and identity accepted): `202 Accepted` when stored
- `POST /api/v0.1/pipeline_stats`: MessagePack DSM pipeline stats: `202 Accepted` when stored
- Malformed payloads return `400 Bad Request` (unless inside the stats failure-injection window, which returns `500`)
- Request bodies above the 2 MiB wire-body limit return `413 Payload Too Large` without being decoded

## Failure injection semantics

With `MOCK_INTAKE_FAIL_STATS_FIRST_N=N`, the first N stats *attempts* return 500 and their payloads are not captured by the intake. Attempts are counted per server instance, atomically, so concurrent requests cannot exceed the limit. Rejected attempts are still decoded, summarized, and dumped, so a rejected attempt and its successful retry are each visible with their own status. Trace and DSM requests do not consume the rejection budget.

## Request summaries and stats grouping

Each handled request produces one summary line on stderr (prefix `mock-intake:`) with the request identity, endpoint, content encoding (`identity` when absent), response status, and decoded payload count. For stats requests, hits are grouped by the full aggregation key: client service/env/version plus origin context (hostname, container, tags, git/image metadata, process tags, aggregation mode) and grouped dimensions (service, name, resource, type, DB type, HTTP status, gRPC status, synthetics, span kind, trace-root flag, HTTP method/endpoint, service source, peer tags, deprecated span-derived primary tags, and additional metric tags). Measurements (hits, errors, duration, sketches) and delivery metadata are excluded from the key, so matching dimensions combine across time buckets. Tag lists are sorted in the summary without modifying captured payloads.

## JSON dumps

With `MOCK_INTAKE_DUMP_DIR` set, each successfully decoded request attempt (including rejected stats attempts) writes one JSON file containing the request identity, endpoint, encoding, response status, and decoded payload. Trace payloads are serialized field by field because `AgentPayload` does not implement `Serialize` in `libdd-trace-protobuf` 4.0.1. DSM dumps contain only the fields the crate decodes. Filenames are collision-resistant and never overwrite earlier dumps.

## Smoke procedure

Point the test-mode trace processor at a local mock intake. The `bottlecap-test-mode` binary is added in [#1216](https://github.com/DataDog/datadog-lambda-extension/pull/1216) (not yet merged); build it from a checkout of that branch:

```bash
cd ../<test-mode-worktree>/bottlecap
cargo build --bin bottlecap-test-mode --features test-mode

cd ../<this-checkout>/bottlecap
cargo run --bin mock-intake --features mock-intake

# in another shell:
DD_APM_DD_URL=http://127.0.0.1:8127 \
DD_LAMBDA_EXTENSION_COMPUTE_STATS=true \
DD_SERVICE=mock-intake-smoke DD_ENV=local DD_VERSION=smoke \
./target/debug/bottlecap-test-mode
```

Unset `DD_SERVERLESS_FLUSH_STRATEGY` so flushing is manual, and make sure no ambient proxy intercepts localhost. Once `GET http://127.0.0.1:8126/info` succeeds, POST a MessagePack trace payload to `/v0.4/traces` (with `X-Datadog-Trace-Count`), then `POST /flush`. The `mock-intake:` summary lines should show the trace request and a stats request whose grouped hits match your spans, and the JSON dumps should contain both payloads.
