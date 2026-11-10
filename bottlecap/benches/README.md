# Bottlecap benchmark suite

Criterion benches for the hot paths of the extension. The extension runs at about 0.07 vCPU
at 128 MB, so per-line, per-span and per-invocation costs show up in billed duration. Every
performance fix should come with a bench from this suite, run before and after.

The companion benches for the shared crates live in serverless-components:
`crates/dogstatsd/benches/hotpath.rs` (groups `dogstatsd/{parse,packet,insert,flush,compress,enhanced,ustr}`)
and `crates/datadog-agent-config/benches/load.rs` (groups `config/{get_config,get_config_yaml,replace_rules}`).
Run them with `cargo bench -p dogstatsd` and `cargo bench -p datadog-agent-config`; the
workspace release profile is the same opt-level "z" + LTO. Those benches use the system
allocator, not jemalloc.

## Benches

| Target (`--bench`) | Group ids | What it measures |
|---|---|---|
| `logs` | `logs/listener_parse`, `logs/processor_process`, `logs/processor_parts`, `logs/oom_check`, `logs/stitch`, `logs/aggregator`, `logs/compress` | The Telemetry API body parse, `LambdaProcessor::process` per line (text, JSON object, JSON string), the operations inside it and their alternatives, the OOM check, split-record stitching, the aggregator batch build, the zstd flush encode |
| `traces` | `traces/decode_v04`, `traces/dedup`, `traces/process_traces`, `traces/stats_copy`, `traces/body_size_encoded_len`, `traces/input_clone`, `traces/tag_injection`, `traces/obfuscate_span`, `traces/normalize_and_top_level`, `traces/regex_compile_per_root_span`, `traces/stats`, `traces/flush` | A 2000-span v0.4 POST: decode, span dedup, `process_traces`, the stats copy, tag injection, obfuscation, normalization, regex filters, stats generation, the flush encode and compress |
| `invocation` | `invocation/trigger`, `invocation/payload`, `invocation/lwa` | Trigger identification on the 38 payloads plus a 220 KB SQS and a 100-record DynamoDB event, the payload parse and clones, the LWA re-parse |
| `enhanced_metrics` | `enhanced_metrics/set_max`, `enhanced_metrics/usage_tick`, `enhanced_metrics/per_invocation_reads`, `enhanced_metrics/dynamic_tags`, `enhanced_metrics/metric_new` | `set_max_enhanced_metrics` (2 regex compiles + `/proc/<pid>/limits`), the usage-monitor tick, the per-invocation `/proc` reads, the dynamic tag rebuild, `Metric::new`. Real `/proc` variants run on Linux only; fixture-tree variants run everywhere |
| `config` | `config/get_config`, `config/replace_rules`, `config/processing_rules` | `get_config` with the none, typical and heavy env sets; the replace-rule and processing-rule compiles |
| `init` | `init/ustr`, `init/http_client`, `init/tokio_runtime` | The `Ustr` first use (in a child process, because the interner is global), the HTTP client builds, the runtime build |

Inputs come from the generators in `tests/fixtures/` (see `tests/fixtures/README.md`). Benches
whose id starts with `alt_` measure an alternative from the review, not production code; they
are the target a fix should reach. The `alt_*` log-line benches are lower bounds: they keep the
status mapping and the OOM scan but skip the processing rules, the durable-function handling and
the aggregator insert.

`criterion` is a dev-dependency with `default-features = false`: no `plotters` (so no HTML
report under `target/criterion`, use critcmp or the CLI output) and no `rayon`, which keeps the
benches on one thread; the `std::env::set_var` calls in `config` and `common::setup_env` rely on that.
The benches use jemalloc, as production does (`benches/common/mod.rs`).

## Run

From `bottlecap/`:

```sh
cargo bench                              # everything, production profile (slow link, see below)
cargo bench --bench logs                 # one target
cargo bench --bench logs -- 'processor'  # ids matching a regex
cargo bench -- --quick                   # fewer samples, for a smoke run
cargo bench --no-run                     # compile only
```

`cargo bench` builds with the `bench` profile, which inherits `[profile.release]` from
`Cargo.toml`: `opt-level = "z"`, `lto = true`, `codegen-units = 1`. That is the profile the
extension ships with, so the numbers are comparable with production (cargo forces
`panic = "unwind"` for bench binaries; nothing else differs). The price is the link: a fat-LTO
link of one bench binary takes a few minutes on a laptop.

To iterate on a bench, skip LTO. The numbers are then not the production profile:

```sh
CARGO_PROFILE_BENCH_LTO=false CARGO_PROFILE_BENCH_CODEGEN_UNITS=16 cargo bench --bench logs -- --quick
# or
BENCH_FAST=1 ../scripts/bench-compare.sh save scratch logs -- --quick
```

## Baselines and comparison

`scripts/bench-compare.sh` wraps criterion's `--save-baseline` and
[critcmp](https://github.com/BurntSushi/critcmp) (`cargo install critcmp`):

```sh
../scripts/bench-compare.sh save main                 # run the suite, save baseline "main"
# ... apply a change ...
../scripts/bench-compare.sh save fix logs traces      # only two targets
../scripts/bench-compare.sh compare main fix -t 5     # table, differences over 5 % only
../scripts/bench-compare.sh export main /tmp/main.json
../scripts/bench-compare.sh list
```

Without critcmp, criterion compares on its own: `cargo bench -- --baseline main` prints the
change against the saved baseline after each bench.

`save` writes `target/criterion/baseline-<name>.txt` with the machine, the load average, the
profile (with any `CARGO_PROFILE_BENCH_*`, `CARGO_PROFILE_RELEASE_*` or `RUSTFLAGS` override),
the commit at the start of the run and the cargo arguments, so a number is never read without
its context. If a tracked file changes during the run, the note gets a `git at the end:` line,
because cargo built what was on disk. `save` first removes any older results saved under the
same name, so a baseline holds one run. `export <name> <file.json>` copies the note to
`<file>.txt`, next to the JSON. `scripts/bench-docker-arm64.sh` writes the same note, plus the
image and the container limits: both scripts source `scripts/bench-note.sh`.

An export is machine specific, so keep it out of git. Compare one with `critcmp old.json <new>`
(critcmp accepts a file path in place of a baseline name).

## Noise control

- Run on a quiet machine: close browsers and IDE indexers, and check `uptime`; the load
  average should be below 2 before `save`. The script records it.
- Keep the CPU fixed: on a laptop, plug in the power and let it cool between runs. Criterion's
  warm-up handles frequency ramp-up, not thermal throttling. On Linux, pin the run
  (`taskset -c 2,3 cargo bench ...`) and, if you can, set the governor to `performance`.
- Use the production profile for any number that goes into a PR.
  `BENCH_FAST=1` is for checking that a bench runs.
- Compare on the same machine. Across machines, compare ratios (fix vs main), not absolute
  times.
- Treat changes under 5 % as noise unless criterion reports `p < 0.05` on a quiet machine and
  the change repeats. `critcmp -t 5` hides the small ones.
- Per-line and per-span benches report `Throughput::Elements`. Criterion and critcmp still
  print the time of the whole batch; the throughput is a rate in elements per second. Divide
  the batch time by the element count for the per-element time.
- The `init/ustr` benches spawn a child process per iteration. Their variance is higher; look
  at the median and run them twice.

## Linux arm64 in Docker

The extension ships on Linux arm64 (Graviton). `scripts/bench-docker-arm64.sh` runs the suite
in a `rust:1.93-bookworm` arm64 container (native on Apple Silicon), limited to 2 CPUs like a
Lambda sandbox, with the target directory in `bottlecap/target-linux-arm64/` so the results
never mix with host baselines. It stops on an x86 Docker host, where arm64 runs under
emulation. A custom `RUST_IMAGE` must be Debian-based: the script runs bash in it and installs
protoc with apt-get, and it stops before the run when the image cannot. From the repository root:

```sh
scripts/bench-docker-arm64.sh linux-main                       # whole suite
scripts/bench-docker-arm64.sh linux-main enhanced_metrics      # the /proc benches, Linux only
CARGO_TARGET_DIR=bottlecap/target-linux-arm64 scripts/bench-compare.sh compare linux-main linux-fix
```

A Lambda at 128 MB gets about 0.07 vCPU, so absolute numbers in the container are still
faster than production. The ratios between benches, and between main and a fix, transfer.
A lambda-sim environment (not in this repository) can reuse this image and the fixture
corpus to measure the whole extension under a CPU quota.

## CI

`.github/workflows/bench.yml` runs a non-blocking part of this plan: `bench-build` compiles the
suite on every PR that touches `bottlecap/`, and `bench-measure` (`scripts/ci/bench-measure.sh`)
runs the smoke list of item 2 on PRs with the `bench` label, nightly on `main`, and on demand,
then compares it with the last `main` baseline. The list below is the full plan.

Goal: catch a broken bench and a large regression on every PR without a flaky gate.

1. **On every PR, blocking:** `cargo bench --no-run` with
   `CARGO_PROFILE_BENCH_LTO=false CARGO_PROFILE_BENCH_CODEGEN_UNITS=16` (compile only, a few
   minutes with the cache), and `cargo test --test fixtures_corpus` (the generators). This
   keeps the suite compiling as the code changes.
2. **On every PR, non-blocking job `bench-smoke`:** run a fixed short list with the production
   profile and `--quick`: `logs/processor_process`, `logs/oom_check`,
   `traces/process_traces/default`, `traces/decode_v04`,
   `invocation/trigger/from_value/sqs_event`, `enhanced_metrics/set_max`,
   `config/get_config/typical`. Save the baseline `pr`, download the baseline artifact from the
   last `main` run, and post `critcmp main pr -t 10` as a PR comment (or a job summary). The job
   never fails on a number; it fails only if a bench panics. A shared GitHub runner is noisy,
   so 10 % is the reporting threshold, and the comment says "informational".
3. **On `main`, nightly or per merge:** the full suite with the production profile, baseline
   saved as an artifact (`critcmp --export main > main.json`) and kept 90 days. A second
   nightly job runs `scripts/bench-docker-arm64.sh` on an arm64 runner for the Linux-only
   `/proc` benches. Track the exported JSON over time (a small script or a Datadog metric per
   bench id) and alert on a 20 % move that holds for 3 nights, which filters runner noise.
4. **Cost note:** `rs_ci.yml` runs `cargo clippy --all-targets` for `default`, `fips` and
   `default,test-mode`, so the six bench targets compile three times per PR. The `fips` clippy
   does not run on a Mac (libddwaf-sys aborts at load); check it on Linux before merging.
5. **Hardware note:** GitHub-hosted runners vary between runs. If the numbers matter more
   than the trend, use a self-hosted or dedicated runner and pin the CPU set. Until then, the
   nightly trend and the PR comment are hints; the developer confirms a regression locally
   with `scripts/bench-compare.sh`.

## Review findings to benches

The investigation numbers are in the Confluence page [Bottlecap review: combined findings](https://datadoghq.atlassian.net/wiki/spaces/~93500838/pages/7384761389); the bench CI workflow (`.github/workflows/bench.yml`) has the current numbers.

| Review item | Finding | Bench id |
|---|---|---|
| 1, 3.8 | Config parse cost, by the size of the environment | `config/get_config/{none,typical,heavy,heavy_no_replace_tags}`; sc `config/get_config/*` |
| 3.1, 4.1 | `Ustr` first use allocates the interner at cold start and never frees it | `init/ustr/first_use`, `init/ustr/first_sorted_tags_parse`; sc `dogstatsd/ustr/*` |
| 3.2 | libddwaf binary size | none: binary size, use `cargo bloat` (#1443) |
| 3.3 | Additional-endpoint secrets block init | none: network; harness test T4 |
| 3.4 | Replace rules and log processing rules compiled before `/next` | `config/replace_rules/compile/{1,3}`, `config/processing_rules/compile/3` |
| 3.5 | FIPS CA bundle loaded 2 or 3 times | `init/http_client/*` (default build; run with `--no-default-features --features fips` for FIPS) |
| 3.6 | Register first; the register client build | `init/http_client/{register_client,first_register_client}` |
| 3.7, 3.10, 3.11 | Non-PIE, dependency weight, wrapper script | none: build flags and binary size; lambda-sim measures RSS and page faults |
| 4.2 to 4.5 | Uncollected tasks and unbounded queues grow memory | none: time series, harness test T8 (memory under a 503 storm) |
| 4.6 | `Vec::with_capacity(100)` per context; a new zstd encoder per request | sc `dogstatsd/flush/consume_metrics`, `dogstatsd/compress/{stream_encoder,bulk_compress,bulk_compressor_reused}_level3` |
| 5.0 | Tick burst after a freeze | `enhanced_metrics/usage_tick/*` (cost per tick; the burst count is a lambda-sim test) |
| 5.1 | Trace POST replies after processing | `traces/decode_v04/libdd_from_request_body` + `traces/process_traces/default` (the end-to-end HTTP seed is not ported, see "Not done") |
| 5.2 | Stats deep copy | `traces/stats_copy/{clone,drop}` |
| 5.3 | Tag injection per span; regex filter compiled per span | `traces/tag_injection`, `traces/process_traces/regex_filters_2`, `traces/regex_compile_per_root_span`, alternatives `traces/tag_injection/{reserve_get_mut,root_span_only}` |
| 5.4 | libdd `from_slice` decode and encode without copy | `traces/decode_v04/{libdd_from_request_body,libdd_from_slice,libdd_from_slice_to_owned,rmp_serde_from_slice}`, `traces/flush/{payload_to_vec,prost_encode_agent_payload,encode/current,encode/borrowed_manual}` |
| 5.5 | JSON log line parsed twice, copied, serialized 2 or 3 times | `logs/listener_parse/*`, `logs/processor_process/per_line/*`, `logs/processor_parts/*`, target `alt_raw_value_end_to_end` |
| 5.6 | OOM check: 7 substring scans per line | `logs/oom_check/is_oom_error_7_contains/{200,2000}`, target `alt_single_regex` |
| 5.7 | Fixed fields serialized per line | `logs/processor_parts/serialize_intake_log`, target `alt_prebuilt_suffix_serialize` |
| 5.8 | The `/proc` poll | `enhanced_metrics/usage_tick/*`, `enhanced_metrics/per_invocation_reads/*` |
| 5.9 | `set_max_enhanced_metrics` cost | `enhanced_metrics/set_max/*` |
| 5.10 | Payload parsed then copied 3 or 4 times, plus the LWA re-parse | `invocation/payload/{parse,clone,parse_plus_3_clones}/*`, `invocation/trigger/from_value/*`, `invocation/lwa/parse/*`, target `payload/typed_partial_parse` |
| 5.11 | Dual-shipped traces not compressed | `traces/flush/zstd/{1,3}` (the cost of compressing) |
| 5.12 | Metrics flush copies series and sketches 2 or more times | sc `dogstatsd/flush/{series_clone,sketch_payload_clone,series_to_json,sketch_to_protobuf}` |
| 5.13 | Connection pooling off | none: network; lambda-sim |
| 5.14 | zstd inline; quadratic stitching | `logs/compress/*`, `logs/stitch/*`, `logs/aggregator/*` |
| 5.15 | opt-level 3 vs z | the whole suite with `CARGO_PROFILE_BENCH_OPT_LEVEL=3`, compared with `scripts/bench-compare.sh compare` |
| 5.16 | Dedup round trip per span; dedup window too small | `traces/dedup/{service_round_trip/{no_timeout,timeout_5s},inline_mutex}` and the printed caught count |
| 5.17 | Stats: a copy and a channel message per span | `traces/stats/{generator_send,send_and_drain}`, `traces/process_traces/compute_stats` |
| 5.18 | DogStatsD: sketch merge vs insert, hashing, dedup before sort | sc `dogstatsd/insert/{sketch_merge_single_point,sketch_insert}/{1,64,2000}_distinct`, `dogstatsd/insert/{count_100_contexts,count_vacant_10000_contexts,distribution_10_contexts}`, `dogstatsd/parse/*`, `dogstatsd/packet/20_lines` |
| 5.19, 5.22 | Size cap no-op; Managed Instance actor throughput | none: correctness and load test |
| 5.20 | Proxy tee body copies 3 times | not covered yet (proxy users only), see "Not done" |
| 5.21 | Enhanced metric tags rebuilt many times per invocation | `enhanced_metrics/dynamic_tags/{rebuild_3_tags,cached_clone}`, `enhanced_metrics/metric_new/distribution`; sc `dogstatsd/enhanced/*` |
| 5.23 | No hot-path benchmarks | this suite |
| 9.4 patch 0003 | `zstd::bulk`, no deep copies per metrics flush | sc `dogstatsd/compress/*`, `dogstatsd/flush/*` |
| 9.4 patch 0006 | Replace rules compiled on first use | `config/replace_rules/compile/*`, `config/get_config/heavy` |
| 9.4 `ustr` branch | Lower RSS after the first parse | `init/ustr/*`; sc `dogstatsd/ustr/*` |
| 9.4 config loader | Faster config load | `config/get_config/*` |

### Not done

- The end-to-end HTTP round trip of a trace POST (seed `bench2_http_end_to_end`) needs a
  `TraceAgent` with `InvocationProcessorHandle::noop()`, which only exists under the
  `test-mode` feature. Port it as `traces/http_round_trip` with `required-features = ["test-mode"]`.
- The proxy tee body (5.20) has no bench; `proxy/tee_body.rs` needs a hyper body fixture.
- Memory benches (RSS, allocations) are one-off prints, not criterion series. A counting
  allocator like the one in the dogstatsd seed would make them tracked numbers.

## Items exposed for the benches

Production code changed only to make these reachable; each is `#[doc(hidden)] pub` and has
no behavior change:

| Item | Why |
|---|---|
| `extension::telemetry::stitch` (module), `FragmentBuffer`, `Stitch`, `FragmentBuffer::stitch` | `logs/stitch` and the fixture test feed split bodies to the stitcher |
| `logs::lambda::processor::is_oom_error` | `logs/oom_check` measures the 7-substring scan on its own |
| `proc::{get_network_data, get_cpu_data, get_uptime, get_fd_max_data, get_fd_use_data, get_threads_max_data, get_threads_use_data}_from_path` | `enhanced_metrics` runs the `/proc` readers on fixture trees, so the bench works on macOS |
