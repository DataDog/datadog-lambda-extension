# Lambda record/replay prototype plan

Goal: record a Lambda function's invocations and outbound HTTP(S) calls in the
cloud with the Datadog extension, then replay them deterministically on
localhost or in CI. No AWS access is needed during replay.

Baseline: `datadog-lambda-extension` main at `ee197984`.

## What exists today

| Piece | Where | How we use it |
|---|---|---|
| Runtime API proxy on `127.0.0.1:9000` (axum + hyper) | `bottlecap/src/proxy/interceptor.rs` | Already tees the `/next` event body (`TeeBodyWithCompletion`) and intercepts `/response` and `/error`. Recording invocations is another consumer of this. |
| Proxy gating | `bottlecap/src/proxy/mod.rs` `should_start_proxy` | Add a record-mode condition. |
| `datadog_wrapper` (`AWS_LAMBDA_EXEC_WRAPPER`) | `scripts/datadog_wrapper` | Already reroutes `AWS_LAMBDA_RUNTIME_API`. It only affects the runtime process, so it is the right place to set `HTTPS_PROXY` and the CA bundle variables without looping the extension's own traffic. |
| Fake Lambda control plane | `bottlecap/tests/common/fake_extensions_api.rs` | Extensions API, Telemetry API sender and `/next` scripting. This is most of what the replay driver needs. |
| SigV4 signing | `bottlecap/src/secrets/decrypt.rs` | Reuse it for uploading recordings to S3 (no AWS SDK dependency). |
| TLS stack | `rustls`, `tokio-rustls`, `hyper` | MITM proxy. Needs `rcgen` for certificate minting. |

## Architecture

### Record mode (in the cloud, inside bottlecap)

```
runtime ──/next,/response,/error──▶ bottlecap :9000 ──▶ Lambda Runtime API
   │                                 (existing interceptor + Recorder)
   └──HTTP(S)_PROXY──▶ bottlecap MITM :9100 ──▶ upstream (AWS, 3rd-party APIs)
                        (new, TLS-terminating, per-sandbox CA)

Recorder ─▶ /tmp/dd-replay/<sandbox>/NNNN.jsonl ─▶ S3 (after each invocation)
```

- Enabled by `DD_REPLAY_RECORD=true` plus `AWS_LAMBDA_EXEC_WRAPPER=/opt/datadog_wrapper`.
- Attribution: on-demand mode handles one invocation at a time, so every
  outbound exchange between `/next` returning and `/response`/`/error` belongs
  to that request ID. Exchanges before the first `/next` belong to `init`.
  Lambda Managed Instances are out of scope (concurrent invocations make
  attribution ambiguous). Recording is refused in that mode.
- CA: minted at extension init (ECDSA P-256 via `rcgen`). Bottlecap writes
  `/tmp/dd-replay/ca-bundle.pem` (system roots + CA) and `ca.pem` (CA only)
  before the runtime starts. Extensions init before the runtime, so the files
  exist when the wrapper runs.
- The wrapper exports (record mode only):
  `HTTP_PROXY`, `HTTPS_PROXY=http://127.0.0.1:9100`,
  `NO_PROXY=127.0.0.1,localhost` (Runtime API, 8126 traces, 8125 statsd, 8124 lifecycle),
  `AWS_CA_BUNDLE`, `REQUESTS_CA_BUNDLE`, `SSL_CERT_FILE` = the full bundle
  (they replace the trust store), `NODE_EXTRA_CA_CERTS` = CA only (it adds),
  and `NODE_USE_ENV_PROXY=1`.
- Bottlecap's own HTTP clients do not see the wrapper environment, so
  extension traffic to Datadog is never recorded.
- Upload: after each invocation's `/response`, the segment is PUT to
  `s3://$DD_REPLAY_BUCKET/<function>/<sandbox-id>/<seq>.jsonl` during
  post-invocation time. SHUTDOWN is not guaranteed, so we don't wait for it.

### Replay mode (on localhost, new `lambda-replay` binary in the bottlecap crate)

```
lambda-replay run <recording-dir> --image <function image or base image + code>

  replay driver
   ├─ Fake Lambda control plane: Runtime API (serves recorded events),
   │  Extensions API + Telemetry API (synthesized platform.* events)
   ├─ Replay MITM :9100 (serves recorded responses, strict matching)
   ├─ Fake intake (collects spans from the real bottlecap)
   ├─ spawns real bottlecap (normal mode, pointed at the fakes)
   └─ spawns the runtime bootstrap with AWS_LAMBDA_RUNTIME_API=<driver>
        + the same proxy/CA env + fake AWS credentials
```

- Bottlecap runs unmodified in replay. Recording is the only change to the
  production binary. The driver reuses the harness's fake control plane,
  promoted from `tests/common` into a `replay` library module.
- Runs inside one container built from the Lambda base image plus the user's
  code and layers. Expected behavior to verify in milestone 4: setting
  `AWS_LAMBDA_RUNTIME_API` makes `/lambda-entrypoint.sh` skip the RIE and run
  the bootstrap directly.
- Output: a report with three diffs per invocation:
  1. the function response (canonical JSON),
  2. the outbound call sequence (missing, extra or changed requests),
  3. the span tree, recorded vs. replayed (resource names, order, errors). This
     needs the recording to include spans; see milestone 5.

## Recording format (JSONL, one record per line)

```jsonc
{"type":"session","version":1,"sandbox_id":"…","function":"…","runtime":"python3.12","env":{/* allowlisted */},"cold_start":true}
{"type":"init_exchange", "seq":0, "req":{…}, "resp":{…}}
{"type":"invocation","request_id":"…","seq":3,"headers":{/* Lambda-Runtime-* */},"event":"<base64|json>"}
{"type":"exchange","request_id":"…","seq":4,"req":{"method":"POST","url":"https://dynamodb.us-east-1.amazonaws.com/","headers":{…},"body":"…"},"resp":{"status":200,"headers":{…},"body":"…"},"started_ms":12,"duration_ms":8}
{"type":"result","request_id":"…","kind":"response|error","body":"…"}
```

- Bodies are stored as UTF-8 when valid, base64 otherwise. Credential headers
  (`Authorization`, `X-Amz-Security-Token`, `Cookie`) are dropped at record
  time.
- Sequence numbers give the total order within the sandbox.

## Matching (replay)

- Key: `(method, scheme+host, path, sorted query, normalized body hash)`.
  Normalization canonicalizes JSON bodies and ignores headers by default (opt-in
  allowlist).
- Each key has a FIFO queue of recorded responses, which handles repeated
  identical calls and concurrent calls that finish in a different order.
- Modes:
  - `strict` (default): an unmatched request returns `599` with a diagnostic
    body and fails the run.
  - `passthrough`: an unmatched request goes to the real service and is
    reported.
- Calls left unconsumed at the end of an invocation are reported as "missing".
- Known non-determinism handled by normalization, not control: SigV4 headers
  and `X-Amz-Date`, trace propagation headers, SDK idempotency/client tokens,
  and retry/invocation-ID headers (`amz-sdk-invocation-id`, `amz-sdk-request`).

## Status (2026-11-09)

Milestones 1–4 are working natively on macOS. `local_tests/replay/run.sh`
records three invocations of a stdlib Python function (one plain-HTTP call and
one real HTTPS call each) with bottlecap in record mode behind the real
`datadog_wrapper`. It then replays them offline with the upstream stopped:
the unchanged replay passes, and a simulated code change fails with the
unexpected request named.

Built differently from the plan:
- The replay control plane is new code in `src/bin/lambda-replay/control_plane.rs`.
  The harness fakes were not moved into the library.
- The MITM forwards upstream with reqwest, not hyper.
- `lambda-replay record` was added. It plays Lambda locally with real network
  access, so recordings can be made without deploying, e.g. against dev AWS
  resources.
- The `before_next_event` hook in `extension::next_event` waits for S3
  uploads before the sandbox can be frozen.

Real Lambda, tested 2026-11-09 in the serverless sandbox (`local_tests/replay/aws-demo/`):
- Python 3.12 arm64 function, layer built with `scripts/build_bottlecap_layer.sh`.
- 3 invocations were recorded (DynamoDB PutItem/GetItem, S3 PutObject, a third-party
  HTTPS API) and uploaded to S3.
- The offline replay on macOS passed, with local boto3 and Python 3.10 instead of
  Lambda's runtime.
- With the simulated change, each invocation failed with
  `unexpected POST dynamodb (PutItem)`, closest recorded call #2,
  `$.Item.status: added`.
- Extensions do not see `_HANDLER` or `AWS_EXECUTION_ENV`, so replay needs
  `--handler` for now.

Dev-loop features (2026-11-09):
- **Ignore rules** (`replay/rules.rs`): `rules.json` in the recording, or `--rules`.
  Rules are selected by host, AWS operation or URL path prefix, and drop JSON
  paths (`$.a.b`, `[*]`, `..key`) or query parameters (`?nonce`) from request
  matching. `ignore_response` drops paths from the response diff.
- **Handler/runtime capture**: `datadog_wrapper` writes `_HANDLER`,
  `AWS_EXECUTION_ENV` and `LAMBDA_TASK_ROOT` to `/tmp/dd-replay/runtime.env`.
  The recorder adds them to the session record, so new recordings don't need
  `--handler`.
- **`--update`** (`replay/player.rs` `updated_session`): recorded calls are
  reused, calls the recording lacks go to the real upstream with host
  credentials, and the session is rewritten. Unused calls are dropped and the
  new results are kept.
- **Recording as fixture** (`lambda-replay serve` and
  `local_tests/replay/python/lambda_replay.py`): pytest calls the handler
  in-process while `serve` answers its calls. Tests can target a recorded
  invocation (by index) or a new one named with an event.
  `LAMBDA_REPLAY_UPDATE=1` records what's missing. An unrecorded call raises
  `ReplayMismatch`, which shows the closest recorded call.
- Demos: `local_tests/replay/run.sh` (rules, change detection, `--update`),
  `local_tests/replay/fixture_demo.sh` (pytest plus update), and
  `local_tests/replay/aws-demo/tests` (pytest against the real Lambda
  recording, 3 tests in 0.4s).
- Known limits: one recording per pytest process (`start()` is a singleton);
  `--update` against the AWS demo is not yet exercised.

Not yet done:
- A process-level harness test for record mode.
- Clock and RNG shims (determinism stage 2).
- Milestones 5 and 6.

## Milestones

Prototype only, not merging to main: no PR-size limit, no cargo feature gate.
All work is on branch `darcy.rayner/lambda-replay-prototype` in the extension repo.

1. **Recording format + matcher** (`bottlecap/src/replay/{format,matcher}.rs`)
   - Serde types, JSONL writer/reader, normalization, FIFO matcher.
   - Unit tests only. No behavior change.
2. **MITM proxy core** (`replay/mitm.rs`)
   - HTTP `CONNECT` handling, per-host leaf certs signed by the in-memory CA
     (cache by SNI), TLS termination, forward via hyper, and an exchange
     callback.
   - Plain-HTTP absolute-form requests too.
   - Test: a local TLS upstream, a client trusting the CA, and an asserted
     recorded exchange.
3. **Record mode wiring**
   - `DD_REPLAY_RECORD` config, a `should_start_proxy` condition, a Recorder
     hooked into `invocation_next_proxy`/`invocation_response_proxy`/`invocation_error_proxy`,
     CA bundle files, `datadog_wrapper` env exports, and S3 segment upload (reusing the
     SigV4 helper).
   - Process-level test in the existing harness: fake API plus a fake upstream,
     asserting the recorded file.
4. **Replay driver** (`src/bin/lambda-replay/`)
   - Move the fake control plane out of `tests/common` into the library.
   - Serve recorded events, synthesize telemetry, run the replay MITM, spawn
     bottlecap and the runtime, and write the response/outbound diff report.
   - Demo: Python function using boto3 (DynamoDB + S3) plus `requests` to a third-party API.
5. **Span-tree diff**
   - Record mode also writes the spans bottlecap receives on 8126 (per request
     ID) into the recording.
   - Replay compares them with spans from the fake intake: structural diff on
     `(service, name, resource, error)` ordered by start.
6. **Node runtime**
   - Verify `NODE_USE_ENV_PROXY` with AWS SDK v3 and `fetch`.
   - Document the runtime/version matrix and its gaps.

## End-to-end validation

1. Deploy a test function (Python 3.12, built layer) with record mode enabled.
2. Invoke it about 5 times with different events (a cold start plus warm
   invocations), then pull the S3 recordings.
3. Run `lambda-replay run` locally with networking disabled (`--network none`) and
   expect the run to pass with identical responses.
4. Change the handler (for example, add a field to a DynamoDB `PutItem`) and expect
   strict replay to fail, with a diff pointing at the changed request body.

## Out of scope for the prototype

- Non-HTTP protocols (Postgres, Redis, etc.). Later options: TCP byte recording
  or tracer-level capture.
- Clock/RNG control. Normalization covers the prototype.
- Java/.NET/Go: Java needs `-Dhttps.proxyHost` via `JAVA_TOOL_OPTIONS` and a
  truststore import. Go honors the proxy env vars natively, which makes it a
  good next candidate.
- Lambda Managed Instances, sampling, PII redaction beyond credential headers,
  and storing recordings in Datadog.

## Decisions

- Record mode ships in the normal binary, with no cargo feature gate.
- No `serverless-components` coordination for now. Code lives in `bottlecap/src/replay/`.
- The replay driver is a second bin in the bottlecap crate (`src/bin/lambda-replay/`).
