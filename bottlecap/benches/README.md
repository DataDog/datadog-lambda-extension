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
| `traces` | `traces/decode_v04`, `traces/dedup`, `traces/process_traces`, `traces/stats_copy`, `traces/tag_injection`, `traces/stats`, `traces/flush` | A 2000-span v0.4 POST: decode, span dedup, `process_traces`, the stats copy, tag injection, obfuscation, normalization, regex filters, stats generation, the flush encode and compress |
| `invocation` | `invocation/trigger`, `invocation/payload`, `invocation/lwa` | Trigger identification on the 38 payloads plus a 220 KB SQS and a 100-record DynamoDB event, the payload parse and clones, the LWA re-parse |
| `enhanced_metrics` | `enhanced_metrics/set_max`, `enhanced_metrics/usage_tick`, `enhanced_metrics/per_invocation_reads`, `enhanced_metrics/dynamic_tags` | `set_max_enhanced_metrics` (2 regex compiles + `/proc/<pid>/limits`), the usage-monitor tick, the per-invocation `/proc` reads, the dynamic tag rebuild. Real `/proc` variants run on Linux only; fixture-tree variants run everywhere |
| `config` | `config/get_config`, `config/replace_rules`, `config/processing_rules` | `get_config` with the none, typical and heavy env sets; the replace-rule and processing-rule compiles |
| `init` | `init/ustr`, `init/http_client`, `init/tokio_runtime` | The `Ustr` first use (in a child process, because the interner is global), the HTTP client builds, the runtime build |

Inputs come from the generators in `tests/fixtures/` (see `tests/fixtures/README.md`). Benches
whose id starts with `alt_` measure an alternative from the review, not production code; they
are the target a fix should reach. The `alt_*` log-line benches are lower bounds: they keep the
status mapping and the OOM scan but skip the processing rules, the durable-function handling and
the aggregator insert.

`criterion` is a dev-dependency with `default-features = false`: no `plotters` (so no HTML
report under `target/criterion`, use critcmp or the CLI output) and no `rayon`, which keeps the
benches on one thread; the `std::env::set_var` calls in `config` and `init` rely on that.

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

To iterate on a bench, skip LTO. The numbers are then not the production profile; the review
measured `opt-level 3` 1.4 to 1.8 times faster than `z` on the logs paths (item 5.15):

```sh
CARGO_PROFILE_BENCH_LTO=false CARGO_PROFILE_BENCH_CODEGEN_UNITS=16 cargo bench --bench logs -- --quick
# or
BENCH_FAST=1 ../scripts/bench-compare.sh save scratch logs -- --quick
```

## Baselines and comparison

`scripts/bench-compare.sh` wraps criterion's `--save-baseline` and
[critcmp](https://github.com/BurntSushi/critcmp) (`cargo install critcmp`):

```sh
scripts/bench-compare.sh save main                 # run the suite, save baseline "main"
# ... apply a change ...
scripts/bench-compare.sh save fix logs traces      # only two targets
scripts/bench-compare.sh compare main fix -t 5     # table, differences over 5 % only
scripts/bench-compare.sh export main /tmp/main.json
scripts/bench-compare.sh list
```

Without critcmp, criterion compares on its own: `cargo bench -- --baseline main` prints the
change against the saved baseline after each bench.

`save` writes `target/criterion/baseline-<name>.txt` with the machine, the load average, the
profile and the commit, so a number is never read without its context.

### First baseline

The first baseline, `mac-main`, is the table at the end of this file; its notes are in
`benches/baselines/mac-main.txt`. The critcmp export (`scripts/bench-compare.sh export`) is
machine specific and 350 KB, so it is not in git (`benches/baselines/*.json` is ignored); CI
keeps exports as artifacts, and a local one compares with `critcmp old.json <new>` (critcmp
accepts a file path in place of a baseline name).

- Commit: `8a2cc021` plus this suite. Branch `jordan.gonzalez/benches/feature`.
- Machine: Apple M4 Max, 16 cores, 64 GB, macOS 26.6. Recorded 2026-10-28 with a 1-minute
  load average of about 5 (other builds were running), so treat the numbers as +-10 %. The
  Linux-only `/proc` benches were skipped; see the Docker recipe.
- Profile: `bench` = release (`opt-level = "z"`, `lto = true`, `codegen-units = 1`).
- Allocator: jemalloc, as in production (`benches/common/mod.rs`).

## Noise control

- Run on a quiet machine: close browsers and IDE indexers, and check `uptime`; the load
  average should be below 2 before `save`. The script records it.
- Keep the CPU fixed: on a laptop, plug in the power and let it cool between runs. Criterion's
  warm-up handles frequency ramp-up, not thermal throttling. On Linux, pin the run
  (`taskset -c 2,3 cargo bench ...`) and, if you can, set the governor to `performance`.
- Use the production profile for any number that goes into a PR or into the table below.
  `BENCH_FAST=1` is for checking that a bench runs.
- Compare on the same machine. Across machines, compare ratios (fix vs main), not absolute
  times.
- Treat changes under 5 % as noise unless criterion reports `p < 0.05` on a quiet machine and
  the change repeats. `critcmp -t 5` hides the small ones.
- Per-line and per-span benches report `Throughput::Elements`, so criterion prints the
  per-element time; use that figure, it is what the backlog quotes.
- The `init/ustr` benches spawn a child process per iteration. Their variance is higher; look
  at the median and run them twice.

## Linux arm64 in Docker

The extension ships on Linux arm64 (Graviton). `scripts/bench-docker-arm64.sh` runs the suite
in a `rust:1.93-bookworm` arm64 container (native on Apple Silicon), limited to 2 CPUs like a
Lambda sandbox, with the target directory in `bottlecap/target-linux-arm64/` so the results
never mix with host baselines:

```sh
scripts/bench-docker-arm64.sh linux-main                       # whole suite
scripts/bench-docker-arm64.sh linux-main enhanced_metrics      # the /proc benches, Linux only
CARGO_TARGET_DIR=bottlecap/target-linux-arm64 scripts/bench-compare.sh compare linux-main linux-fix
```

A Lambda at 128 MB gets about 0.07 vCPU, so absolute numbers in the container are still
faster than production. The ratios between benches, and between main and a fix, transfer.
The lambda-sim environment (`scripts/lambda-sim/`) can reuse this image and the fixture
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
   profile and `--quick` (about 2 minutes of measurement): `logs/processor_process`,
   `logs/oom_check`, `traces/process_traces/default`, `traces/decode_v04`,
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

## Backlog to bench

Review = `consolidated-findings-v3` (Confluence page 7384761389). Numbers are the first
baseline on this Mac (see above), production profile, marked **measured**; a dash means the
bench exists but the number was not recorded yet. Per-element numbers are what to watch.

| Review item | Cost it names | Bench id | Baseline on this Mac |
|---|---|---|---|
| 1, 3.8 | Config parse 18 µs (none), 126 µs (20 vars), 370 µs (45 vars) on Linux arm64 | `config/get_config/{none,typical,heavy,heavy_no_replace_tags}`; sc `config/get_config/*` | **measured** none 23.8 µs, typical 62.1 µs, heavy 978 µs, heavy without `DD_APM_REPLACE_TAGS` 179 µs (the host has 149 env vars; a sandbox has about 32). sc crate alone (`NoExtension`): none 24.1 µs, typical 62.2 µs, heavy 823 µs, typical from `datadog.yaml` 247 µs |
| 3.1, 4.1 | `Ustr` first use writes 9 MiB, 1.4 to 2 ms at cold start; never frees | `init/ustr/first_use`, `init/ustr/first_sorted_tags_parse`; sc `dogstatsd/ustr/*` | **measured** `Ustr::from("")` 747 µs, first `SortedTags::parse` 732 µs; child RSS +9392 KiB (9.2 MiB) on the first use; warm `Ustr::from` x16 163 ns. sc: first parse 655 µs, RSS +8.4 MiB; growth with unique tag values printed once: +1.2 MiB at 1k, +2.4 MiB at 10k, +19.5 MiB at 100k |
| 3.2 | libddwaf 1.8 to 2 MB of binary | none: binary size, use `cargo bloat` (#1443) | n/a |
| 3.3 | Additional-endpoint secrets block init | none: network; harness test T4 | n/a |
| 3.4 | 0.6 ms per replace rule compiled before `/next`; log rules too | `config/replace_rules/compile/{1,3}`, `config/processing_rules/compile/3` | **measured** 1 rule 281 µs, 3 rules 766 µs (inside `get_config`, via `deserialize_apm_replace_rules` and libdd `parse_rules_from_string`); 3 log processing rules 444 µs |
| 3.5 | FIPS CA bundle loaded 2 or 3 times (1.76 ms each on M4) | `init/http_client/*` (default build; run with `--features fips` for the FIPS number) | **measured** (default build, webpki roots) first TLS client in a fresh process 37.8 µs; register + shared + trace client in `main.rs` order 54.1 µs, RSS +528 KiB; warm builds 1.9 to 3.6 µs |
| 3.6 | Register first; the register client build | `init/http_client/{register_client,first_register_client}` | **measured** warm 2.3 µs, first in process 37.8 µs |
| 3.7, 3.10, 3.11 | Non-PIE, dependency weight, wrapper script | none: build flags and binary size; lambda-sim measures RSS and page faults | n/a |
| 4.2 to 4.5 | Uncollected tasks and unbounded queues grow memory | none: time series, harness test T8 (memory under a 503 storm) | n/a |
| 4.6 | `Vec::with_capacity(100)` per context (5.9 MB per 1000); new zstd encoder per request (3.66 MB) | sc `dogstatsd/flush/consume_metrics`, `dogstatsd/compress/{stream_encoder,bulk_compress,bulk_compressor_reused}_level3` | **measured** (sc, system allocator) `consume_metrics` 2.12 µs per context (2.12 ms per 1000); zstd level 3 on 668 KB: stream 135 µs, bulk 129 µs, reused 128 µs; on 6.7 MB: 1.46 / 1.32 / 1.25 ms; CCtx memory printed once: stream 3.66 MB, bulk 1.30 MB |
| 5.0 | Tick burst after a freeze: 36 µs per scan, up to 6000 scans | `enhanced_metrics/usage_tick/*` (cost per tick; the burst count is a lambda-sim test) | **measured** fake `/proc` tree (4 pids, 32 fds, 8 tasks) 194 µs on macOS, where a file open costs about 16 µs; `statfs("/tmp")` 0.85 µs. The real `/proc` tick is Linux-only: not recorded yet, run `scripts/bench-docker-arm64.sh` |
| 5.1 | Trace POST replies after processing: 26 ms per 2000 spans, 16 ms after decode | `traces/decode_v04/libdd_from_request_body` + `traces/process_traces/default` (the end-to-end HTTP seed is not ported, see "Not done") | **measured** decode 10.7 ms (5.4 µs per span) + `process_traces` 11.2 ms (5.6 µs per span) = 21.9 ms of work before the reply, for a 2000-span POST |
| 5.2 | Stats deep copy 3.8 ms per 2000 spans | `traces/stats_copy/{clone,drop}` | **measured** clone 2.4 ms + drop 1.58 ms (1.2 + 0.8 µs per span) |
| 5.3 | Tag injection 2.2 µs per span; regex filter compiled per span | `traces/tag_injection`, `traces/process_traces/regex_filters_2`, `traces/regex_compile_per_root_span`, alternatives `traces/tag_injection/{reserve_get_mut,root_span_only}` | **measured** tag injection 4.1 ms (2.06 µs per span, 17 tags); `process_traces` with 2 regex filters 40.2 ms vs 11.2 ms without (3.6x); 2 `Regex::new` per root span 28.8 ms per 100 roots (144 µs each); `reserve_get_mut` 3.5 ms, root-span-only 0.99 ms |
| 5.4 | libdd `from_slice` 34 % faster; encode without copy 21 % faster | `traces/decode_v04/{libdd_from_request_body,libdd_from_slice,libdd_from_slice_to_owned,rmp_serde_from_slice}`, `traces/flush/{payload_to_vec,prost_encode_agent_payload,encode/current,encode/borrowed_manual}` | **measured** decode: libdd `from_request_body` 10.7 ms, libdd borrowed `from_slice` 2.2 ms, borrowed + convert to `pb::Span` 4.1 ms, `rmp_serde::from_slice` 6.5 ms; flush encode: `to_vec` + `AgentPayload` encode 10.7 ms vs borrowed manual encode 6.3 ms (byte-identical), of which `to_vec` 2.3 ms and prost encode 7.1 ms |
| 5.5 | JSON log line parsed twice, copied, serialized 2 or 3 times: 32 to 41 µs vs 7 to 9 µs | `logs/listener_parse/*`, `logs/processor_process/per_line/*`, `logs/processor_parts/*`, target `alt_raw_value_end_to_end` | **measured** per line: listener parse (flatten) json_object 12.6 µs, json_string 3.8 µs, text 0.63 µs; `process()` json_object 23.6 µs, json_string 17.0 µs, text 2.8 µs (so 36 µs today for a 2 KB JSON object line); parts: event clone 2.3 µs, `to_string(obj)` 2.4 µs, re-parse 8.9 µs, `IntakeLog` serialize 2.5 µs; lower bound `alt_raw_value_end_to_end` json_object 6.8 µs (parse + extract + status + OOM scan + one serialize; no rules, durable handling or aggregator insert), `raw_value_record` parse 2.2 µs |
| 5.6 | OOM check 7 scans per line: 1 to 4 µs vs under 0.4 µs | `logs/oom_check/is_oom_error_7_contains/{200,2000}`, target `alt_single_regex` | **measured** 1.02 µs (200 B), 3.5 µs (2000 B), 0.40 µs on a hit; one regex 0.031 µs and 0.19 µs |
| 5.7 | Fixed fields serialized per line: 0.6 to 0.9 µs | `logs/processor_parts/serialize_intake_log`, target `alt_prebuilt_suffix_serialize` | **measured** text 0.77 µs, JSON 2.5 µs per line; prebuilt suffix 0.18 µs and 1.8 µs |
| 5.8 | `/proc` poll every 10 ms: 26 to 36 µs | `enhanced_metrics/usage_tick/*`, `enhanced_metrics/per_invocation_reads/*` | **measured** on macOS fixtures only: `/proc/stat` 16.4 µs, `/proc/net/dev` 16.2 µs, `/proc/uptime` 16.1 µs (file-open bound). Linux numbers pending (Docker recipe) |
| 5.9 | `set_max_enhanced_metrics` 485 to 506 µs vs 54 µs | `enhanced_metrics/set_max/*` | **measured** fixture tree 367 µs, of which the 2 `Regex::new` are 236 µs; hoisted regex 70.5 µs; `strip_prefix` 64 µs; `Lambda::set_max_enhanced_metrics` on macOS (empty pid list) 242 µs |
| 5.10 | Payload parsed then copied 3 or 4 times: 519 µs (220 KB SQS) + 557 µs LWA | `invocation/payload/{parse,clone,parse_plus_3_clones}/*`, `invocation/trigger/from_value/*`, `invocation/lwa/parse/*`, target `payload/typed_partial_parse` | **measured** SQS 220 KB: parse 480 µs (noisy; 383 µs in a second run), `Value` clone 10 µs (10 large strings), parse + 3 clones 436 µs, LWA re-parse 1.11 ms, typed partial parse 202 µs; DynamoDB 100 records (126 KB): parse 1.10 ms, clone 308 µs, parse + 3 clones 2.9 ms, LWA 2.6 ms; `IdentifiedTrigger::from_value` 1.3 to 9.2 µs on the 37 checked-in events (msk with headers 21 µs), 12.6 µs on SQS 220 KB, 505 µs on DynamoDB 100 records; it runs up to 3 times per invocation |
| 5.11 | Dual-shipped traces not compressed | `traces/flush/zstd/{1,3}` (the cost of compressing) | **measured** 1.84 MB protobuf: zstd level 1 1.65 ms (to 146 KB), level 3 1.80 ms (to 143 KB); `SendData::send` with a fake HTTP client 12.7 ms in total |
| 5.12 | Metrics flush copies series and sketches 2 or more times: 4.7 MB, 1.5 ms per 2000 contexts | sc `dogstatsd/flush/{series_clone,sketch_payload_clone,series_to_json,sketch_to_protobuf}` | **measured** (sc) per context: series clone 415 ns, sketch payload clone 222 to 274 ns, series to JSON 815 ns, sketch to protobuf 527 to 586 ns; `consume_distributions` 1.34 µs |
| 5.13 | Connection pooling off: 2 to 6 ms per flush | none: network; lambda-sim | n/a |
| 5.14 | zstd inline 10 to 17 ms per 5 MiB; stitching 4 MB 100 to 181 ms | `logs/compress/*`, `logs/stitch/*`, `logs/aggregator/*` | **measured** zstd stream level 3: 2.26 MB noisy batch 6.8 ms (316 MB/s; synthetic text compresses 140x and runs at 4.3 GB/s, so use the noisy row), level 1 2.1 ms; stitch one 1 MiB record 16.6 ms, 4 MiB 88.5 ms, 4 MiB across 3 POSTs 128 ms; the failed full parse before it 0.77 ms; aggregator 10k lines 481 µs (48 ns/line) |
| 5.15 | opt-level 3 vs z, 1.4 to 1.8x on logs | the whole suite with `CARGO_PROFILE_BENCH_OPT_LEVEL=3`, compared with `scripts/bench-compare.sh compare` | — |
| 5.16 | Dedup round trip per span, window of 100 too small | `traces/dedup/{service_round_trip/{no_timeout,timeout_5s},inline_mutex}` and the printed caught count | **measured** channel round trip 692 µs per 2000 spans (346 ns per span), 1.62 ms with the 5 s timeout (810 ns); inline `Mutex<Deduper>` 171 µs (86 ns); a resend of the same 2000 spans catches 0 duplicates with capacity 100 |
| 5.17 | Stats: a copy and a channel message per span, 5.3 µs | `traces/stats/{generator_send,send_and_drain}`, `traces/process_traces/compute_stats` | **measured** `StatsGenerator::send` 2.2 ms (1.1 µs per span), send + concentrator drain 6.3 ms (3.2 µs per span); `process_traces` with compute_stats 10.6 ms |
| 5.18 | DogStatsD: merge 30 to 685 ns vs insert 7 to 94 ns; hash 264 vs 10 ns; dedup before sort | sc `dogstatsd/insert/{sketch_merge_single_point,sketch_insert}/{1,64,2000}_distinct`, `dogstatsd/insert/{count_100_contexts,count_vacant_10000_contexts,distribution_10_contexts}`, `dogstatsd/parse/*`, `dogstatsd/packet/20_lines` | **measured** (sc) merge 43.5 / 173 / 684 ns vs insert 6.9 / 27.5 / 93.9 ns per value (6 to 7x); count insert 70.8 ns per metric (occupied), 341 ns (vacant, with rehash); distribution insert 643 ns; parse count 328 ns, distribution 8 tags 1.12 µs, `SortedTags::parse` 8 tags 678 ns, 16 enrich tags 1.69 µs, `metric::id` 250 ns; packet of 20 lines 8.3 µs (416 ns per line) |
| 5.19, 5.22 | Size cap no-op; Managed Instance actor throughput | none: correctness and load test | n/a |
| 5.20 | Proxy tee body copies 3 times, 2 to 4 ms per 6 MB | not covered yet (proxy users only), see "Not done" | — |
| 5.21 | Enhanced metric tags rebuilt 15 to 26 times per invocation: 530 ns vs 22 ns | `enhanced_metrics/dynamic_tags/{rebuild_3_tags,cached_clone}`, `enhanced_metrics/metric_new/distribution`; sc `dogstatsd/enhanced/*` | **measured** rebuild 499 ns vs cached clone 18.7 ns; `Metric::new` 176 ns |
| 5.23 | No hot-path benchmarks | this suite | — |
| 9.4 patch 0003 | `zstd::bulk`, no deep copies per metrics flush | sc `dogstatsd/compress/*`, `dogstatsd/flush/*` (before numbers) | **measured** (sc) see 4.6 and 5.12; the stream encoder costs 5 to 15 % more time and 2.8x the memory of bulk |
| 9.4 patch 0006 | Replace rules compiled on first use | `config/replace_rules/compile/*`, `config/get_config/heavy` (before numbers) | **measured** heavy 978 µs vs 179 µs without replace tags: the 3-rule compile is about 0.8 ms of a heavy `get_config` |
| 9.4 `ustr` branch | RSS +9.3 MiB to +144 KiB after the first parse | `init/ustr/*`; sc `dogstatsd/ustr/*` (before numbers) | **measured** 747 µs and +9.2 MiB on the first use |
| 9.4 config loader | 30 to 190 µs and 150 KB saved | `config/get_config/*` (before numbers) | **measured** none 23.8 µs, typical 62.1 µs, heavy 978 µs |

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
