# Fault-injection test harness

Process-level contract tests run the real `bottlecap` binary against a fake
Lambda control plane and a Datadog intake that misbehaves on demand. Each test
asserts what Lambda or the customer would see: `/next` is called before the
deadline, the process exits within the SHUTDOWN deadline, data reaches the
intake, and nothing panics. No test reads internal state.

Every reliability fix should come with one of these tests. A test for a bug that
is not fixed yet is committed `#[ignore = "<ID>: <what the extension does today>"]`,
so CI stays green and the test turns into a regression test when the fix lands
(remove the attribute in the same PR). The ID names the Jira ticket that tracks
the fix (for example `SVLS-10061`). A reason that starts with `needs owner decision` marks
a test whose expected behavior is a design choice, not a plain bug.

## Layout

| File | Role |
|---|---|
| `tests/common/harness.rs` | Umbrella module and `prelude`. |
| `tests/common/fake_extensions_api.rs` | `FakeExtensionsApi`: register, telemetry subscribe (status knob), test-driven `/next`, request log, Telemetry API sender. |
| `tests/common/fault_intake.rs` | `FaultIntake` with `FaultMode`, per-path modes, body-size rejection, request log; `SynBlackhole`. |
| `tests/common/bottlecap_process.rs` | `BottlecapProcess`: spawn the binary with the base env, capture stdout/stderr, RSS/CPU, exit with a deadline, kill on drop. |
| `tests/common/port_guard.rs` | Hold or probe the fixed ports (8124, 8125/udp, 8126, 8999, 9000). |
| `tests/common/telemetry_events.rs` | Telemetry API records as `Value`, thin wrappers over `tests/fixtures/telemetry.rs`. |
| `tests/fixtures/` | The shared corpus for benches and these tests (see its `README.md`). Each test crate declares `mod fixtures;`. |
| `tests/lifecycle_contract_test.rs` | Shutdown and `/next` contracts. |
| `tests/fault_injection_test.rs` | Intake faults: a 503 storm, an oversized trace, a response cut off mid-body. |
| `tests/config_robustness_test.rs` | Wrong-typed config values. |
| `tests/port_conflict_test.rs` | Another process holds port 8124 or 8126. |
| `.config/nextest.toml` | `fixed-ports` test group (`max-threads = 1`) for these binaries. |

## Write a new fault test in 10 lines

```rust
mod fixtures;
#[path = "common/harness.rs"]
mod harness;
use harness::prelude::*;

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
#[ignore = "SVLS-xxxxx: <what the extension does today>"]
async fn my_fault() {
    let s = Scenario::start(FaultMode::Ok).await;          // fake API + intake, fixed ports checked
    let mut bc = s.bottlecap().spawn();                     // real binary, base env points at the fakes
    assert!(s.invoke("req-1", secs(10), 1).await);          // wait for /next #1, answer with INVOKE
    let invoked = Instant::now();                           // the invoke deadline runs from here
    s.intake.set_mode(FaultMode::Status(503));              // the fault
    deliver_runtime_done(&s.api, "req-1", true).await.unwrap();
    let deadline = invoked + secs(10);                      // /next #2 must arrive by it
    assert!(s.api.wait_for_next_calls(2, deadline).await, "{}", bc.log_tail(40));
    s.shutdown_cleanly(&mut bc).await;                      // SHUTDOWN, exit 0 within 2 s, no panic
}
```

## Rules

- Never sleep a fixed time. Wait on a recorded event with a deadline:
  `api.wait_for_next_calls`, `intake.wait_for(...)`, `bc.wait_for_exit`.
- Assert the customer-visible contract (a `/next` call, an exit, an intake
  request), never a log line. Print `bc.log_tail(40)` and `s.intake.summary()`
  in the message. One exception: a log line may prove that the fault was really
  injected (the port-conflict tests check that the held port conflicted), never
  that the contract holds.
- Bound a flush by the invocation or shutdown deadline, not by a number from
  an implementation.
- Always check `bc.assert_no_panic()` (or use `shutdown_cleanly`). Dev builds
  unwind and keep running, release builds abort; both print `panicked at`.
- Check memory growth only on Linux (`BottlecapProcess::can_measure_memory()`).
- Send `platform_init_start()` once per cold start, as Lambda does, in a test
  that checks logs: until it arrives the extension holds every log.
- Every spawning test carries `#[serial_test::serial(bottlecap_ports)]`: the
  binary binds fixed ports, and plain `cargo test` runs tests in parallel. New
  test binaries must also match the regex in `.config/nextest.toml`.
- Run with a per-worktree `CARGO_TARGET_DIR`: the binary under test is the
  uplifted `target/debug/bottlecap`, which a shared target dir overwrites.
  `log_tail` prints the binary path and age.

## API summary

- `FakeExtensionsApi`: `push_invoke(id, deadline_in)`, `push_shutdown(reason, deadline_in)`,
  `next_calls()`, `next_call_at(n)`, `wait_for_next_calls(n, deadline)` (true only when `/next` call `n` arrived by
  the `Instant`), `set_subscribe_status(StatusCode)`, `request_lines()`,
  `send_telemetry(&[Value], budget)`, `send_telemetry_raw(Vec<u8>, budget)`, `telemetry_port()`
  (from the last accepted subscribe body, `None` before one; `send_telemetry` fails without it,
  as Lambda delivers telemetry only to subscribers). Both post to the port and path of the
  subscribed URI. Like Lambda, `send_telemetry` withholds
  events whose type (`platform`, `function`, `extension`) the subscribe body did not ask for;
  `send_telemetry_raw` posts its body as is. Like Lambda, `/next` serves only the event types from
  the register body and drops the rest, a `/next` while another is pending gets `403`, `/next`
  and subscribe get `403` without the `Lambda-Extension-Identifier` issued at register, register
  gets `403` without `Lambda-Extension-Name: datadog-agent` or with an invalid body, a subscribe
  body outside the Telemetry API reference rules gets `400` (also an `https://` URI, port 9001,
  or a missing `Content-Type: application/json`), and
  `accountId` needs `Lambda-Extension-Accept-Feature`. `set_managed_instance()` applies the Lambda
  Managed Instances rules: register gets `403` for `INVOKE`, and subscribe gets `400` for any
  `schemaVersion` but `2025-01-29`. The SHUTDOWN reason is lower case (`spindown`), as the Runtime
  Interface Emulator sends it.
  Any other path gets `202`.
- `FaultIntake` (HTTP/1.1): modes `Ok`, `Status(code)`, `Slow(d)`, `Blackhole`,
  `RetryAfter{status, secs}`, `CloseMidBody`; string forms `ok`, `status:<c>`, `slow:<ms>`,
  `blackhole`, `retry_after:<c>:<s>`, `close_mid_body`.
  `Blackhole` never reads a body: with a path mode set, it reads only the request head to pick
  the mode, and records the request with `body_len` 0. An upload that breaks off is not
  recorded and gets no answer.
  `set_mode`, `set_path_mode(path, mode)`, `clear_path_mode`, `set_reject_over(Some(bytes))`,
  `set_ok_body(path, body)`; `requests()`, `requests_to`, `count`, `connections()`, `summary()`,
  `wait_for(n, pred, budget)`, `wait_for_path`; `IntakeRequest::api_key()`, `decoded_body()`
  and `body_text()` (`Err` when the body does not decode). `SynBlackhole::start()` (backlog 1,
  queue pre-filled), `is_saturated()`.
- `BottlecapProcess`: `scenario.bottlecap()` then `.intake(url)`, `.env`, `.envs`, `.remove_env`,
  `.managed_instance()` (also switches the fake API to its rules), `.flush_strategy(s)`,
  `.secret_from(credentials_url)`, `.spawn()`; `wait_for_exit(budget)` (kills on timeout),
  `is_running()`, `kill()`, `stdout()`, `stderr()`, `log_tail(n)`, `log_contains`, `panic_lines()`,
  `assert_no_panic()`, `rss_kb()`, `hwm_kb()`, `cpu_ms()`, `uptime()`; `STARTUP_BUDGET`,
  `EXIT_BUDGET`, `TEST_API_KEY`.
- `Scenario`: `start(mode)`, `bottlecap()`, `invoke(id, deadline, poll)` (waits for the poll,
  then answers it), `wait_next_or_exit`, `shutdown(&mut bc)` (asserts the process is still
  running when SHUTDOWN is queued and exits within the 2 s SHUTDOWN deadline),
  `shutdown_cleanly(&mut bc)` (also asserts exit 0 and no panic).
  Helpers: `deliver_runtime_done`, `msgpack_trace(trace_id, meta_bytes)`, `post_trace` (to 8126),
  `incompressible_text`, `secs`, `ms`. Records: `platform_init_start`, `platform_start`,
  `platform_runtime_done(id, status, with_metrics)`, `platform_report`, `function_log`,
  `platform_logs_dropped`, `unknown_event`; larger bodies come from `fixtures::telemetry`.
- `PortGuard::tcp(port)` / `udp(port)` / `*_wait` bind the address the extension uses.

## Base environment

`AWS_LAMBDA_RUNTIME_API=<fake api>`, `AWS_LAMBDA_FUNCTION_NAME=test`,
`AWS_LAMBDA_FUNCTION_MEMORY_SIZE=128`, `AWS_LAMBDA_FUNCTION_VERSION=$LATEST`,
`AWS_REGION`/`AWS_DEFAULT_REGION=us-east-1`, `LAMBDA_TASK_ROOT=<tempdir>`,
`DD_API_KEY=<TEST_API_KEY>`, `DD_SITE=datadoghq.com`, `DD_LOG_LEVEL=debug`,
`DD_SERVERLESS_FLUSH_STRATEGY=end`,
`DD_DD_URL`/`DD_URL`/`DD_APM_DD_URL`/`DD_LOGS_CONFIG_LOGS_DD_URL=http://<intake>`,
`DD_HTTP_PROTOCOL=http1` (the fakes speak HTTP/1.1; the default client uses h2
prior knowledge), `DD_PROXY_HTTPS=http://127.0.0.1:1` (trace stats follow
`DD_SITE` over https; an https-only proxy on a closed port fails them at once
while the `http://` intake URLs bypass it). Managed Instance mode adds
`AWS_LAMBDA_INITIALIZATION_TYPE=lambda-managed-instances`.

## Running

```sh
cd bottlecap
cargo nextest run --test lifecycle_contract_test --test fault_injection_test \
    --test config_robustness_test --test port_conflict_test            # CI set, ignored skipped
cargo nextest run --test fault_injection_test --run-ignored only          # not-yet-fixed tests (none yet)
cargo test --test fault_injection_test                                    # also serialized
```

`.config/nextest.toml` puts these binaries in the `fixed-ports` group
(`max-threads = 1`); nextest runs each test in its own process, so the
`serial` attribute alone would not protect the ports across binaries. A test
fails fast with `fixed ports still busy before spawn` when a leaked process
holds one.

## Hand-offs

- The fault modes belong in the shared `datadog-mock-intake` crate once
  bottlecap consumes it: a `FaultMode` enum with the string forms above,
  `set_mode`/`set_path_mode`, body-size rejection, and the recorded status and
  mode per request. `blackhole` needs the accept loop, so the crate must expose
  it. Until then `fault_intake.rs` stays here next to the shared crate's payload
  decoding.
- A secret-recovery test (the secret fetch fails, then recovers) needs the extension to honor
  `AWS_ENDPOINT_URL_<SERVICE>` / `AWS_ENDPOINT_URL` (the AWS SDK convention) for
  its Secrets Manager call; that hook is a separate small change.
