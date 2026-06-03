# APMSVLS-487 — respect `Datadog-Client-Computed-Stats` (branch `lpimentel/respect-client-computed-stats`)

## Goal

When a tracer sends `Datadog-Client-Computed-Stats: <truthy>`, the extension must:
1. Skip its own (agent-side) stats generation for those traces.
2. Signal the backend not to compute stats by **leaving `_dd.compute_stats` absent** on each span's meta.
3. Do the same for the extension-generated `aws.lambda` span (Path B), which requires
   propagating the flag from the tracer's placeholder span.

## Canonical semantics (validated against the Go agent)

`datadog-agent/cmd/serverless-init/tag/tag.go` + `pkg/serverless/tags/tags.go` only ever set
`_dd.compute_stats = "1"`, **never `"0"`**, and leave the key absent otherwise:
- `"1"` → backend computes stats (only when nobody else did: not tracer, not extension).
- absent → backend does not compute (someone already did, or accepted-incorrect).

This **supersedes** the earlier "override to `0`" plan. `"0"` and absent are equivalent
"off" states to the backend, so emitting absent instead of today's `"0"` is **not a regression**;
it just aligns us with the canonical agent.

Rule (one line): **set `_dd.compute_stats="1"` iff `!compute_trace_stats_on_extension && !client_computed_stats`; otherwise leave absent.**

### Truth table

| `compute_trace_stats_on_extension` | `client_computed_stats` | `_dd.compute_stats` on span meta | Extension computes stats? |
|---|---|---|---|
| false | false | `"1"` | No |
| false | true | absent | No |
| true  | false | absent | Yes |
| true  | true  | absent | No |

## Header-value finding (cross-runtime)

`Datadog-Client-Computed-Stats` is not standardized across tracers:
- `"true"` — .NET, Java, PHP, Python
- `"yes"` — JS, Ruby, C++
- `"t"` — Go

bottlecap consumes the **already-parsed** `client_computed_stats` bool from
`libdd_trace_utils` (`tracer_header_tags.rs:137`): `client_computed_stats = !value.is_empty()`.
All three values are non-empty → all map to `true` → **the fix triggers on every runtime.**
No production change needed here; we just consume the bool.

### Latent divergence vs the Go agent (NOT in scope — follow-up)

The Go agent applies one uniform rule to both `client_computed_stats` and
`client_computed_top_level` via `isHeaderTrue` (`pkg/trace/api/api.go:1065`, used at
`:947`/`:1055`): empty → `false`; `strconv.ParseBool` success → that bool (so
`"0"`/`"false"`/`"f"` → `false`); ParseBool failure (e.g. `"yes"`) → `true`.

libdatadog (`tracer_header_tags.rs:133-138`) parses the two headers **differently from
the agent and from each other**:
- `client_computed_stats` (`:136-137`): `!value.is_empty()` — non-empty → `true`.
- `client_computed_top_level` (`:133-135`): `headers.get(...).is_some()` — **presence-only**;
  value ignored, so even an empty value → `true`.

| Header value | libdd stats | libdd top_level | Go agent (both) |
|---|---|---|---|
| absent | false | false | false |
| `""` (present, empty) | false | **true** | false |
| `"true"`/`"yes"`/`"t"`/`"1"` | true | true | true |
| `"0"`/`"false"`/`"f"` | **true** | **true** | false |

So `top_level` is the looser of the two (diverges on empty-present **and** falsey strings);
`stats` diverges only on falsey strings. Both are latent: tracers signal by sending a
truthy/present header and omit it otherwise, so the divergent rows don't occur in real traffic.

- [ ] **Follow-up libdatadog PR**: make `From<&HeaderMap>` use `isHeaderTrue`/`ParseBool`
  semantics for **both** `client_computed_stats` and `client_computed_top_level` to match the
  agent (also collapses libdatadog's internal stats-vs-top_level inconsistency). Shared
  libdatadog, separate from this ticket. (This ticket only consumes `client_computed_stats`.)

## Implementation (production code)

- [ ] `bottlecap/src/tags/lambda/tags.rs`
  - Keep `COMPUTE_STATS_KEY` `pub(crate)`.
  - **Stop baking** `_dd.compute_stats` in `tags_from_env` (remove the unconditional insert).
    This also drops it from `_dd.tags.function` (correct — it's a per-span backend directive).
  - Update unit tests: `test_new_from_config` (`tags_map.len()` 3→2, delete `COMPUTE_STATS_KEY=="1"`),
    `test_get_function_tags_map*` (`14→13`), add a test asserting the key is absent from `get_tags_map()`.
- [ ] `bottlecap/src/traces/trace_processor.rs`
  - `use crate::tags::lambda::tags::COMPUTE_STATS_KEY;`
  - Add `client_computed_stats: bool` to `struct ChunkProcessor`.
  - In `ChunkProcessor::process`, after the `tags_map` copy: insert `_dd.compute_stats="1"`
    **only when** `!self.config.compute_trace_stats_on_extension && !self.client_computed_stats`.
  - In `process_traces`, set `client_computed_stats: header_tags.client_computed_stats`.
  - In `send_processed_traces`, capture `client_computed_stats` before `header_tags` is moved and
    add `&& !client_computed_stats` to the `compute_trace_stats_on_extension` stats-gen guard.
  - Update the 4 in-test `ChunkProcessor { … }` literals with `client_computed_stats: false`.
- [ ] `bottlecap/src/lifecycle/invocation/context.rs` (Path B)
  - Add `pub client_computed_stats: bool` to `struct Context` (+ doc) and init `false` in `Default`.
  - `ContextBuffer::add_tracer_span`: add `client_computed_stats: bool` param; set on context.
- [ ] `bottlecap/src/lifecycle/invocation/processor.rs` (Path B)
  - `add_tracer_span`: add param, pass through.
  - `send_ctx_spans`: read `context.client_computed_stats` before `get_ctx_spans` consumes it.
  - `send_spans` (`processor.rs:743`): add param. It already builds a `TracerHeaderTags` and hardcodes
    `client_computed_stats: false` (`:759`) before calling `send_processed_traces` → `process_traces` →
    `ChunkProcessor`. Set that field from the param so Path B **reuses the Tier-1 `ChunkProcessor` logic**
    to stamp `_dd.compute_stats` (single source of truth — no separate manual meta insert).
  - `send_cold_start_span`: pass `false`.
- [ ] `bottlecap/src/lifecycle/invocation/processor_service.rs` (Path B)
  - Add `client_computed_stats: bool` to `ProcessorCommand::AddTracerSpan`, the handle method,
    and the command handler.
- [ ] `bottlecap/src/traces/trace_agent.rs` (Path B source)
  - Annotate `let tracer_header_tags: TracerHeaderTags<'_> = (&parts.headers).into();`
  - At the placeholder-span branch (`span.resource == INVOCATION_SPAN_RESOURCE`), pass
    `tracer_header_tags.client_computed_stats` to `add_tracer_span`.

## Tests

**Tests-first**: each tier is red → green. Write the failing test(s), then make the
minimal production change above that turns them green. Run `cargo nextest` for the
touched module after each tier.

- [ ] **Tier 0 — header-parsing contract** (bottlecap, at `(&headers).into()` boundary, `trace_agent.rs:506`)
  - `"true"`, `"yes"`, `"t"` → `client_computed_stats == true` (all runtimes trigger the fix).
  - `""`, absent → `false`.
  - **Included (documenting test):** `"false"`/`"0"` → currently `true`; assert-and-comment to
    surface the known libdatadog divergence intentionally and point at the follow-up.
  - Green on existing code (no production change) — locks the contract the rest relies on.
- [x] **Tier 1 — `trace_processor.rs` unit** ✅
  - `tags.rs`: `COMPUTE_STATS_KEY` now `pub(crate)`; removed unconditional insert in `tags_from_env`
    (no longer leaks into `_dd.tags.function`). Updated `test_new_from_config` (3→2 + absence assert)
    and both `test_get_function_tags_map*` (14→13 + absence assert).
  - `trace_processor.rs`: imported `COMPUTE_STATS_KEY`; added `client_computed_stats: bool` to
    `ChunkProcessor`; stamp `_dd.compute_stats="1"` in `process` only when
    `!compute_trace_stats_on_extension && !client_computed_stats`; wired field from
    `header_tags.client_computed_stats` in `process_traces`; added `&& !client_computed_stats` to the
    stats-gen guard in `send_processed_traces` (captured before `header_tags` is moved). Updated the
    4 in-test `ChunkProcessor` literals + `test_process_trace` expected span.
  - New tests: `test_compute_stats_truth_table` (asserts on `Span.meta`, #1118 guard) and
    `test_stats_skip_guard_via_send_processed_traces` (drives real `StatsConcentratorService`,
    asserts flushed payload Some only for `(compute_on_extension=true, client_computed_stats=false)`).
  - Fixed integration tests that asserted the old leak: removed `_dd.compute_stats:1` from
    `logs_integration_test` body assertions; corrected the `metrics_integration_test` comment.
  - All 548 workspace tests pass; fmt + clippy clean.
- [x] **Tier 2 — `processor.rs` Path B** ✅
  - `context.rs`: added `pub client_computed_stats: bool` to `Context` (+ doc, `Default` init false);
    `ContextBuffer::add_tracer_span` takes a `client_computed_stats` param and records it on the context.
  - `processor.rs`: `add_tracer_span` threads the param through; `send_ctx_spans` reads
    `context.client_computed_stats` before `get_ctx_spans` consumes it; `send_spans` takes the param and
    sets it on its `TracerHeaderTags` so Path B reuses the Tier-1 `ChunkProcessor` logic (single source of
    truth); `send_cold_start_span` passes `false`.
  - `processor_service.rs`: `ProcessorCommand::AddTracerSpan` + handle method + command handler carry
    `client_computed_stats`.
  - `trace_agent.rs`: annotated `tracer_header_tags: trace_utils::TracerHeaderTags<'_>` and passes
    `tracer_header_tags.client_computed_stats` to `add_tracer_span` at the placeholder-span branch.
  - Tests: `test_add_tracer_span_sets_client_computed_stats` (context.rs) and
    `test_send_ctx_spans_stamps_compute_stats` (processor.rs — drives Path B end-to-end through the
    `trace_tx` channel and asserts on the `aws.lambda` span's `_dd.compute_stats` for all 4 truth-table rows).
  - All 550 workspace tests pass; fmt + clippy clean.
- [x] **Tier 3 — full fake-intake E2E** (`bottlecap/tests/apm_integration_test.rs`, harness from PR #1194) ✅
  - Added `run_processor_pipeline(compute_on_extension, client_computed_stats)` which routes a trace
    through `SendingTraceProcessor::send_processed_traces`, drains the `trace_tx` channel into
    `AggregatorService` (synchronously, post-send, to avoid a flush race), flushes via `TraceFlusher`,
    and flushes stats via `StatsConcentratorService` → `StatsAggregator` → `StatsFlusher`. Parameterized
    `header_tags_with(client_computed_stats)`. Tests: T3.1 (`e2e_client_computed_stats_leaves_compute_stats_absent`),
    T3.2 (`e2e_compute_stats_truth_table_on_captured_span`), T3.3 (`e2e_stats_suppressed_unless_extension_computes`),
    T3.4 (`e2e_client_computed_stats_absent_meta_and_no_stats`). All assert on the captured `AgentPayload`
    span meta and `stats_payloads()`. 554/554 workspace tests pass; fmt + clippy clean.
  - ⚠️ The existing `trace_payload_roundtrip_through_fake_intake` flushes a hand-built
    `pb::TracerPayload` and **never runs `process_traces`**, so it can't observe `_dd.compute_stats`.
    New cases MUST route a trace **through `SendingTraceProcessor::send_processed_traces`** before flushing.
  - Wiring per test:
    - `AggregatorService::default()` + spawn `.run()`. `SendingTraceProcessor.trace_tx` is an
      `mpsc::Sender<SendDataBuilderInfo>` (distinct from `AggregatorHandle`): make a channel, pass `tx`
      as `trace_tx`, spawn a forwarder `while let Some(info)=rx.recv().await { agg_handle.insert_payload(info) }`.
      Flush via `TraceFlusher::new(agg_handle, …).flush(None)` → `fake_intake.trace_payloads()`.
    - `StatsConcentratorService::new(config)` + spawn; `StatsGenerator::new(conc_handle)` as `stats_generator`.
      After `send_processed_traces`, flush concentrator → `StatsAggregator` → `StatsFlusher` (mirror the
      existing `stats_payload_roundtrip_*` test) → `fake_intake.stats_payloads()`.
    - `SendingTraceProcessor { appsec: None, processor: Arc::new(ServerlessTraceProcessor { obfuscation_config }), trace_tx, stats_generator }`.
    - Parameterize `header_tags()` (currently hardcodes `client_computed_stats: true`, `apm_integration_test.rs:43`)
      and `Config.compute_trace_stats_on_extension`.
  - T3.1: `client_computed_stats=true` → captured `AgentPayload` span meta has `_dd.compute_stats` **absent**.
  - T3.2 control: absent when `compute_on_extension=true` OR `client_computed_stats=true`; `"1"` for the neither row.
  - T3.3: stats suppression — empty `stats_payloads()` when `client_computed_stats=true`; one payload for
    `(compute_on_extension=true, client_computed_stats=false)`.
  - T3.4: combined path — trace meta absent AND zero stats payloads.

## Validation

```bash
cd bottlecap
cargo fmt --all
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --features default
cargo nextest run --workspace
```

## ⚠️ Risk carried from the ticket (not solved by this code)

Go/Java tracers flush client-side stats async, which often doesn't complete in Lambda before the
extension forwards data. With this fix those runtimes could see zero stats (extension skips +
backend told to skip + tracer never flushed). Full fix chosen anyway.
- [ ] **Gate manual E2E** on the exact golang-default/java cases that failed before
  (`test_trace_aws_lambda_hits_metric`) prior to merge.
