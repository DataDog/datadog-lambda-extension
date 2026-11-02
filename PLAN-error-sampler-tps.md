# Plan: rate-limit Lambda error trace rescue with `DD_APM_ERROR_TPS` and deprecate `DD_SERVERLESS_ERROR_SAMPLER_ENABLED`

> Temporary planning file (untracked). Delete before merging the PR. This file plans **PR 1 of 3**; PRs 2 and 3 are listed under "Follow-up PRs" and are out of scope here.

## PR series
| PR | Scope | Base |
|---|---|---|
| **1 (this plan)** | `DD_APM_ERROR_TPS` budget, `RateLimited` mode, deprecate `DD_SERVERLESS_ERROR_SAMPLER_ENABLED`. Rescue gating unchanged: only when the extension computes stats. | `main` |
| 2 | Also run rescue when the tracer computes stats (`Datadog-Client-Computed-Stats: true`). On that path rescue only stamps `_dd.errors_sr` on kept errored P0 chunks; every chunk is still forwarded, so the backend's P0 drop keeps the stamped ones (same model as SCL, APMSVLS-472). | PR 1 |
| 3 | DataDog/datadog-lambda-extension#1358, rebased onto PR 2 and trimmed to filtering sampled-out chunks when the tracer computes stats. | PR 2 |

Order matters: filtering on the tracer path before rescue exists there would drop errored P0 chunks unrescued. Today tracers still send P0 chunks because `/info` reports `client_drop_p0s: false` (`bottlecap/src/traces/trace_agent.rs:653`).

## Goal
Make `DD_APM_ERROR_TPS` the only knob for error trace rescue in bottlecap. Rescue always uses the shared `ErrorSamplerMode::RateLimited` sampler with Go agent / SCL trace-agent defaults: rescue is on at 10 TPS when nothing is set, and `<= 0` turns it off. `DD_SERVERLESS_ERROR_SAMPLER_ENABLED` stays parsed but deprecated.

## Context (verified by reading the code)
- Repo: DataDog/datadog-lambda-extension. Worktree: `/home/lucas/source/datadog/datadog-lambda-extension--lpimentel-error-sampler-rate-limited`. Branch: `lpimentel/error-sampler-rate-limited` at `aa9d3f54`, 1 commit behind `origin/main` (`e36a4709`, #1424, `AGENTS.md` only). Fast-forward to `origin/main` before starting; no line references below change. All Rust code is under `bottlecap/`. Toolchain is 1.93.1.
- `AGENTS.md` (after #1424) says to check `serverless-components` before adding functionality. This plan adds no new shared logic: the sampler is already the shared crate, and the new code is bottlecap's `LambdaConfig` extension field plus its resolution (bottlecap-specific because of the deprecated flag). SCL's `parse_error_sampler_config` lives in `datadog-trace-agent`, not in a shared crate, so there is nothing to reuse.
- Sampler crate `datadog-agent-trace-sampler` is pinned at serverless-components rev `e462105e918904e6b1ad2ebc153ae561cebe5a34` (`bottlecap/Cargo.toml:100`). Do not bump it. Its source is at `~/.cargo/git/checkouts/serverless-components-d7345b4db0f8d2aa/e462105/crates/datadog-agent-trace-sampler/src/{lib.rs,score_sampler.rs}`. What it provides:
  - `ErrorSamplerConfig { mode, target_tps, extra_sample_rate }`. `Default` is `RateLimited`, `10.0`, `1.0`.
  - `ErrorsSampler` is a public enum with variants `AlwaysKeep { disabled }` and `RateLimited(Box<RateLimitedSampler>)`. Tests can `matches!` on it.
  - `is_disabled()` returns `target_tps <= 0.0`.
  - `sample(now_unix_secs, &TraceView)` rotates its buckets only from the `now_unix_secs` argument. `bucket_index` uses `rem_euclid`. A backward clock step (`new_bucket < prev`) zeroes nothing and does not panic. A gap longer than 30s zeroes every bucket.
- Config loading: `datadog-agent-config` builds the YAML source and then the env source. For each source it runs `figment.extract::<LambdaConfigSource>()` and calls `LambdaConfig::merge_from`.
  - Env provider: `Env::prefixed("DD_")`. A source field named `apm_error_tps` therefore maps to `DD_APM_ERROR_TPS` and to the flat YAML key `apm_error_tps`.
  - The core `Config` has no `error_tps` field, so the new field does not collide with anything.
  - The `merge_fields!` `option:` group copies the value only when the source value is `Some`.
- Figment parses env values as follows (`figment-0.10.19/src/value/parse.rs`):
  - `"5"` and `"-1"` become integers, `"2.5"` becomes f64, and `" 7 "` is trimmed.
  - `""`, `"abc"`, `"inf"`, `"nan"` and `"1e400"` stay strings. `deserialize_option_lossless::<f64>` then warns and returns `None`.
  - `"1.0e999"` contains a `.`, so it parses to f64 `inf` and passes `deserialize_option_lossless`.
  - YAML `.inf` and `.nan` also arrive as non-finite f64.
  - **A finiteness filter is therefore required after deserialization.**
- Logging is initialized before config load (`bottlecap/src/bin/bottlecap/main.rs:116` vs `:164`), so warnings from the deserializer are visible.
- Lints: `bottlecap/src/lib.rs:2-10` denies `clippy::all`, `clippy::pedantic`, `clippy::unwrap_used`, `unreachable_pub` and `missing_copy_implementations`. `clippy::float_cmp` is pedantic; the repo convention is `#[allow(clippy::float_cmp)]` on the test (see `src/metrics/enhanced/lambda.rs:876`). Pedantic also requires `#[must_use]` on value-returning pub fns and backticks around identifiers in doc comments.
- SCL reference: DataDog/serverless-components branch `lpimentel/add-error-sampler`, `crates/datadog-trace-agent/src/config.rs`, function `parse_error_sampler_config`. It starts from `ErrorSamplerConfig::default()` and only overrides `target_tps`. Blank, unparsable and non-finite values warn and fall back to the default.

## Settled semantics (do not reopen)
1. `DD_APM_ERROR_TPS` (f64, YAML `apm_error_tps`) is the budget. Mode is always `RateLimited` and `extra_sample_rate` is always `1.0`. Bottlecap can never select `AlwaysKeep`.
2. With nothing set, the target is `10.0`, which equals `ErrorSamplerConfig::default().target_tps`. A target `<= 0` disables rescue.
3. `DD_SERVERLESS_ERROR_SAMPLER_ENABLED` is deprecated but still parsed. Warn whenever it is explicitly set. `true` is redundant (target 10). `false` with no usable TPS is a kill switch (target 0).
4. If both are set, TPS wins, and the warning names both values. This holds regardless of which source (YAML or env) each came from.
5. An invalid, blank or non-finite TPS warns and is ignored for that source only. Resolution then uses any remaining valid TPS from a lower-priority source (YAML under env), then the legacy flag, then 10.0. Example: YAML `apm_error_tps: 5` + env `DD_APM_ERROR_TPS=abc` + `DD_SERVERLESS_ERROR_SAMPLER_ENABLED=false` resolves to 5.
6. Large finite values (for example `1000000000`) are accepted as-is, no cap and no warning (Go/SCL parity).
7. Configuration keys: env `DD_APM_ERROR_TPS` (parity with the Go trace-agent's env var) and flat YAML `apm_error_tps`. Go's YAML key `apm_config.errors_per_second` is intentionally not supported: parity target is env vars, not YAML files.
8. Deprecation warnings do not promise removal (no "will be removed" wording).
9. No "setting has no effect" warning when compute stats is off: whether the tracer computes stats is known only per request, and PR 2 makes rescue apply there too.

## Constraints
- TDD: write each step's tests first, watch them fail (or fail to compile), then implement.
- Every step must leave the whole crate compiling (`cargo test --lib` builds all lib tests regardless of name filter). Each step therefore updates every caller its type or signature change breaks; do not defer callers to a later step.
- Keep `lambda_extension_compute_stats` gating unchanged (`trace_processor.rs:638`).
- Keep comments concise and leave history out of them. No em dashes in code comments or strings.
- Non-goals: removing `AlwaysKeep` from the shared crate, stats-payload `errorTPS`, changing compute-stats gating (PR 2), tracer-path filtering (PR 3), public docs. The repo has no changelog and no doc mentions `DD_SERVERLESS_ERROR_SAMPLER_ENABLED`.
- Do not commit or push unless the user asks. When asked, make two commits (step 1 alone, then everything else). Commit messages must be user-facing.
- The PR description must not include test counts. Use `.github/pull_request_template.md` (`## Overview`, `## Testing`). No Jira ticket in the PR title; mention APMSVLS-469 in the description.

## Plan

### Step 1: stale `DD_APM_FEATURES` comment (separate commit)
1. Test first: in `bottlecap/src/config/mod.rs` `lambda_config_tests` (next to `apm_features_comma_separated_list_from_env`, ~:767), add `apm_features_space_separated_list_from_env`.
   - Set `DD_APM_FEATURES="some_other_feature error_rare_sample_tracer_drop"`.
   - Assert `config.apm_features == ["some_other_feature", "error_rare_sample_tracer_drop"]` and `error_rare_sample_tracer_drop_enabled(&config)`.
   - It should pass immediately: it is a regression test, because the pinned crate uses `deserialize_comma_then_space_separated_string` (`datadog-agent-config/src/sources/env.rs:237`).
2. In `bottlecap/src/traces/trace_processor.rs:81-84`, replace "parsed upstream as a comma-separated list ... space-separated values are not supported" with this: `DD_APM_FEATURES` is split on commas when one is present, otherwise on spaces, and matching is exact per entry.
3. Verify: run `cd bottlecap && cargo test --lib apm_features`. All `apm_features*` tests pass.

### Step 2: config resolution in `bottlecap/src/config/mod.rs`
1. Tests first, in `lambda_config_tests`:
   - Replace the three tests at ~:718-742 (`serverless_error_sampler_enabled_*`) with table-driven tests. Each case calls `load(|jail| ...)`, then asserts `config.ext.error_sampler_target_tps()` and whether `config.ext.legacy_error_sampler_warning()` is `Some`.
   - Put `#[allow(clippy::float_cmp)]` on the tests, and include the case input in every assert message.

   | Case | Setup | Target | Warning |
   |---|---|---|---|
   | default | nothing set (also assert `apm_error_tps == None`, `serverless_error_sampler_enabled == None`) | 10.0 | None |
   | env int | `DD_APM_ERROR_TPS=5` | 5.0 | None |
   | env float | `DD_APM_ERROR_TPS=2.5` | 2.5 | None |
   | env padded | `DD_APM_ERROR_TPS=" 7 "` | 7.0 | None |
   | YAML | `apm_error_tps: 3` | 3.0 | None |
   | env over YAML | YAML `apm_error_tps: 3` + env `DD_APM_ERROR_TPS=4` | 4.0 | None |
   | zero | `DD_APM_ERROR_TPS=0` | 0.0 | None |
   | negative | `DD_APM_ERROR_TPS=-1` | -1.0 | None |
   | large | `DD_APM_ERROR_TPS=1000000000` | 1e9 | None |
   | invalid env | `DD_APM_ERROR_TPS` = `""`, `"abc"`, `"inf"`, `"nan"`, `"1.0e999"` (assert `apm_error_tps == None`) | 10.0 | None |
   | invalid YAML | `apm_error_tps: .inf`, `apm_error_tps: .nan` (assert `apm_error_tps == None`) | 10.0 | None |
   | legacy true | `DD_SERVERLESS_ERROR_SAMPLER_ENABLED=true` | 10.0 | Some, contains `deprecated` |
   | legacy false | `DD_SERVERLESS_ERROR_SAMPLER_ENABLED=false` | 0.0 | Some, contains `DD_APM_ERROR_TPS=0` (migration hint) |
   | legacy YAML false | `serverless_error_sampler_enabled: false` | 0.0 | Some |
   | both, TPS 5 + legacy false | env both | 5.0 | Some, contains `DD_APM_ERROR_TPS=5` and `DD_SERVERLESS_ERROR_SAMPLER_ENABLED=false` |
   | both, TPS 0 + legacy true | env both | 0.0 | Some, contains `DD_APM_ERROR_TPS=0` and `DD_SERVERLESS_ERROR_SAMPLER_ENABLED=true` |
   | cross-source | YAML `apm_error_tps: 5` + env `DD_SERVERLESS_ERROR_SAMPLER_ENABLED=false` | 5.0 | Some, names both |
   | invalid TPS + legacy false | `DD_APM_ERROR_TPS=abc` + legacy `false` | 0.0 | Some, the kill-switch message, not the both-set one |
   | invalid env over valid YAML + legacy false | YAML `apm_error_tps: 5` + env `DD_APM_ERROR_TPS=abc` + env legacy `false` | 5.0 | Some, the both-set message naming `DD_APM_ERROR_TPS=5` |

   Rust formats `5.0_f64` with `{}` as `5`, so `"DD_APM_ERROR_TPS=5"` matches.

   Before finalizing the table, check how figment treats exponent notation without a dot (`DD_APM_ERROR_TPS=1e3`). The `1e400` row above suggests it stays a string and is rejected as invalid. Add a row pinning whatever actually happens, and if it is rejected, list it under Risks so the PR can mention it.

   - Add `default_apm_error_tps_matches_shared_sampler_default`, asserting `DEFAULT_APM_ERROR_TPS == datadog_agent_trace_sampler::ErrorSamplerConfig::default().target_tps`.
   - Update `apm_features_error_rare_sample_tracer_drop_from_env` (~:752-764). Replace `assert!(!config.ext.serverless_error_sampler_enabled)` with `assert_eq!(config.ext.error_sampler_target_tps(), DEFAULT_APM_ERROR_TPS)`. Reword its comment to "The feature only widens rescue eligibility; it must not change the rescue budget."
   - Verify: `cargo test --lib lambda_config_tests` fails to compile (red).
2. Add `pub const DEFAULT_APM_ERROR_TPS: f64 = 10.0;` near the top of the LambdaConfig section. Its doc comment should say it matches the shared sampler and Go agent default.
3. Add a private deserializer `deserialize_optional_finite_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error>`.
   - Call `deser_opt_lossless::<_, f64>(d)?`, then filter out non-finite values.
   - When a value is dropped, call `tracing::warn!("Ignoring non-finite DD_APM_ERROR_TPS value {v}")` (fully qualified, since `tracing` is not imported in this file).
4. `LambdaConfig` (~:72-79, default ~:147):
   - Change `serverless_error_sampler_enabled` to `Option<bool>` and give it default `None`.
   - Rewrite its doc: deprecated raw value of `DD_SERVERLESS_ERROR_SAMPLER_ENABLED`; read it only through `error_sampler_target_tps` and `legacy_error_sampler_warning`.
   - Add `pub apm_error_tps: Option<f64>` with default `None`. Its doc should cover:
     - `DD_APM_ERROR_TPS`: the rescue budget in errored traces per second, distributed fairly across trace signatures; `<= 0` disables rescue.
     - The budget is per execution environment, so N concurrent sandboxes can rescue up to N times the budget.
     - Rescue currently runs only when the extension computes stats (`lambda_extension_compute_stats`). Word this as current behavior; PR 2 widens it.
     - The limit is approximate, with no hard ceiling (inherited from the Go agent and SCL). Rates are recomputed on 5s bucket rotation over a 30s window. A cold sampler keeps errors at rate 1.0 until volume in the current bucket exceeds about 5x the budget. After a freeze/thaw gap, counters expire but a throttled signature's rate recovers by at most 20% per evaluation. See Risks for why this wording differs from "unknown signatures sample at 1.0".
   - `PartialEq` still derives fine with an f64 field.
   - Keep the type change compiling: in `bottlecap/src/startup.rs`, change `!config.ext.serverless_error_sampler_enabled` (:172) to `config.ext.serverless_error_sampler_enabled != Some(true)` and the argument at :181 to `config.ext.serverless_error_sampler_enabled == Some(true)`. These are temporary; steps 3 and 4 replace them. Fix any other non-test reader the compiler reports the same way.
5. `LambdaConfigSource` (~:213-214): keep `serverless_error_sampler_enabled` (still `deser_opt_bool`). Add `#[serde(deserialize_with = "deserialize_optional_finite_f64")] pub apm_error_tps: Option<f64>` with a one-line `DD_APM_ERROR_TPS` doc.
6. `merge_from` (~:297): move `serverless_error_sampler_enabled` from the `value:` list to the `option:` list, and add `apm_error_tps` to `option:`.
7. In `impl LambdaConfig` (~:347), add two methods, both `#[must_use]` and `pub`:
   - `error_sampler_target_tps(&self) -> f64`:
     ```
     match (self.apm_error_tps, self.serverless_error_sampler_enabled) {
         (Some(tps), _) => tps,
         (None, Some(false)) => 0.0,
         _ => DEFAULT_APM_ERROR_TPS,
     }
     ```
   - `legacy_error_sampler_warning(&self) -> Option<String>`:
     - `None` when the legacy flag is `None`.
     - (TPS `Some`, legacy `Some(b)`): `"DD_SERVERLESS_ERROR_SAMPLER_ENABLED={b} is deprecated and ignored because DD_APM_ERROR_TPS={tps} is set"`.
     - (TPS `None`, `true`): `"DD_SERVERLESS_ERROR_SAMPLER_ENABLED is deprecated and has no effect: error trace rescue is on by default at {DEFAULT_APM_ERROR_TPS} traces per second; use DD_APM_ERROR_TPS to change the budget"`.
     - (TPS `None`, `false`): `"DD_SERVERLESS_ERROR_SAMPLER_ENABLED is deprecated; set DD_APM_ERROR_TPS=0 to disable error trace rescue instead"`.
     - No em dashes, and no removal promise in any message. Add a test asserting none of the three messages contains `removed`.
8. Verify: `cargo build --all-targets` compiles, and `cargo test --lib lambda_config_tests` passes, including `defaults_match_lambda_config_default`.

### Step 3: sampler construction in `bottlecap/src/traces/trace_processor.rs`, plus every caller
1. Tests first, in `mod tests`:
   - `error_sampler_config_is_rate_limited_with_given_tps`: for tps in `[10.0, 2.5, 0.0, -1.0]`, assert `error_sampler_config(tps)` has `mode == ErrorSamplerMode::RateLimited`, `target_tps == tps` and `extra_sample_rate == 1.0`. Use `#[allow(clippy::float_cmp)]`.
   - `new_error_sampler_builds_rate_limited`: assert `matches!(*new_error_sampler(10.0).lock().expect("lock"), ErrorsSampler::RateLimited(_))`. For `[0.0, -1.0]` assert `is_disabled()`. For `10.0` assert `!is_disabled()`.
   - `error_sampler_survives_clock_jumps`:
     - Build `new_error_sampler(DEFAULT_APM_ERROR_TPS)` and a single errored `SpanView` (the crate tests show the shape).
     - Call `.sample(now, &trace)` in sequence with now = `1_700_000_000`, `1_699_999_000` (backward), `1_700_100_000` (long gap), `0` (the `UNIX_EPOCH.elapsed()` fallback).
     - Use distinct trace ids (`i.wrapping_mul(2_654_435_761)`).
     - Assert no panic, and that the call after the long gap returns `SampleDecision::Keep { errors_sr: 1.0 }` (cold, unthrottled sampler).
   - `error_sampler_stays_throttled_after_long_gap` (documents inherited behavior, deterministic timestamps):
     - Build `new_error_sampler(1.0)`. Drive one errored signature with 200 traces in each of several consecutive 5s buckets (timestamps `t0 + 5*k`, distinct trace ids) so its stored rate drops well below 1.0.
     - Then sample once at `t_last + 3600` (gap far beyond the 30s window).
     - Assert no panic and that the returned decision, if `Keep`, has `errors_sr < 1.0`: stale counters are cleared, but the stored rate only recovers by at most 1.2x per evaluation (`MAX_RATE_INCREASE`, `score_sampler.rs:21,101-107`). If the hash makes the single decision a `Drop`, use a few distinct trace ids and assert the first `Keep` has `errors_sr < 1.0`.
   - `error_sampler_budget_is_shared_across_invocations` (shared processor versus a deliberately reset control):
     - Build `create_compute_stats_config()`.
     - Shared: one `create_test_processor(&config, new_error_sampler(1.0), false)`, then 10 separate `process_traces` calls (one per simulated invocation). Each call carries 50 single-span errored P0 traces of the same signature with distinct nonzero trace ids (`(call * 50 + i + 1).wrapping_mul(2_654_435_761)`).
     - Control: the same 10 batches and ids, but a fresh `create_test_processor(&config, new_error_sampler(1.0), false)` per call.
     - Count rescued spans for each with the existing `trace_spans` helper (`trace_processor.rs:1599`).
     - Assert `shared > 0` and `shared * 2 < control`. A deterministic simulation of the pinned sampler gives about 29 shared versus about 160 reset, so the margin is wide. A bucket rotation mid-test only lowers `shared` further, since rotation applies the throttled rate.
     - Verify the test discriminates: temporarily make the shared run build a fresh sampler per call and confirm the assertion fails, then revert.
   - In `test_error_sampler_rescues_errored_p0_chunks` (~:1477), add an assert that the rescued root's `_dd.errors_sr` is `1.0` (cold sampler, low volume). Add `#[allow(clippy::float_cmp)]`.
   - In `test_disabled_error_sampler_drops_errored_p0_chunks` (~:1827), loop over `[0.0, -1.0]` to cover the `is_disabled` short-circuit with a negative budget. Change "(the shipping default)" in its doc (~:1825) to "(`DD_APM_ERROR_TPS <= 0`)".
   - Verify: tests fail to compile (red).
2. Replace `new_error_sampler(enabled: bool)` (~:119-138):
   - Add `#[must_use] pub fn error_sampler_config(target_tps: f64) -> ErrorSamplerConfig`. It returns `ErrorSamplerConfig { mode: ErrorSamplerMode::RateLimited, target_tps, extra_sample_rate: 1.0 }`. Set the fields explicitly so a crate default change cannot alter bottlecap behavior.
   - Add `#[must_use] pub fn new_error_sampler(target_tps: f64) -> Arc<std::sync::Mutex<ErrorsSampler>>`, which wraps `ErrorsSampler::new(error_sampler_config(target_tps))`.
   - Rewrite the doc comment. Drop the wrong freeze/thaw claim. Say instead:
     - The rolling window is driven by the wall-clock seconds passed on each call, so a freeze/thaw gap expires stale counters. Stored per-signature rates are not reset: a throttled signature recovers by at most 20% per evaluation (inherited from the Go agent).
     - `<= 0` disables rescue.
     - Point to `LambdaConfig::apm_error_tps` for caveats.
3. Field doc (~:96-100): "a stateful mode (`RateLimited`) would keep its rolling window" becomes "Shared across invocations so the `RateLimited` budget and rolling window span the whole execution environment".
4. Test helper (~:792-796): `enabled_error_sampler()` returns `new_error_sampler(crate::config::DEFAULT_APM_ERROR_TPS)`. Change the doc to "Error sampler with the default budget".
5. In the same file, replace `new_error_sampler(false)` with `new_error_sampler(0.0)` at ~:1763, ~:1830 and ~:1862.
6. Update every other caller in this same step so the crate compiles:
   - `bottlecap/src/lifecycle/invocation/processor_service.rs:834` and `bottlecap/src/traces/trace_agent.rs:1148`: `new_error_sampler(false)` becomes `new_error_sampler(0.0)`.
   - `bottlecap/tests/apm_integration_test.rs:378`: `new_error_sampler(true)` becomes `new_error_sampler(bottlecap::config::DEFAULT_APM_ERROR_TPS)`. Check the import path against the file's existing `use bottlecap::...` lines.
   - `bottlecap/src/startup.rs:180-182`: pass `config.ext.error_sampler_target_tps()` (replaces the temporary step 2 argument).
   - `bottlecap/src/lifecycle/invocation/processor.rs:2296,2406,3044,3625` use `enabled_error_sampler()` and need no edit.
7. Verify: `cargo build --all-targets` compiles; `cargo test --lib trace_processor` passes; `ffgrep 'new_error_sampler\((true|false)\)'` on `bottlecap/` returns zero matches.

### Step 4: startup wiring in `bottlecap/src/startup.rs:159-183`
1. Delete the `DD_APM_ERROR_TPS` "not supported" block (:159-166). Change `use std::{env, sync::Arc};` (:15) to `use std::sync::Arc;`, because `env` has no other use in this file.
2. Insert:
   ```
   if let Some(message) = config.ext.legacy_error_sampler_warning() { warn!("{message}"); }
   let error_tps = config.ext.error_sampler_target_tps();
   ```
3. Retarget the rare-sample warning (:172) to `if error_rare_sample_tracer_drop && error_tps <= 0.0`. Message: `"DD_APM_FEATURES includes error_rare_sample_tracer_drop, but error trace rescue is disabled (effective DD_APM_ERROR_TPS {error_tps}): errored traces dropped by the tracer will not be rescued"`.
4. Build the sampler with `trace_processor::new_error_sampler(error_tps)` (reusing the local instead of the step 3 inline call).
5. Do not add a warning for "rescue configured but compute stats off" (settled semantics 9).
6. Verify: `cargo build` succeeds and `cargo test --lib startup` passes. The warning logic itself is covered by the step 2 tests on `legacy_error_sampler_warning` and `error_sampler_target_tps`.

## Files to Modify
- `bottlecap/src/config/mod.rs`:
  - `DEFAULT_APM_ERROR_TPS`, finite-f64 deserializer.
  - `LambdaConfig` field type change plus `apm_error_tps`; source field; `merge_from`.
  - `error_sampler_target_tps` and `legacy_error_sampler_warning`; tests.
- `bottlecap/src/traces/trace_processor.rs`:
  - Stale `DD_APM_FEATURES` doc (step 1).
  - `error_sampler_config` and `new_error_sampler(f64)`; docs; test helper; new and updated tests.
- `bottlecap/src/startup.rs`: real resolution, deprecation and precedence warning, retargeted rare-sample warning, drop the `env` import.
- `bottlecap/src/lifecycle/invocation/processor_service.rs`: `new_error_sampler(0.0)`.
- `bottlecap/src/traces/trace_agent.rs`: `new_error_sampler(0.0)`.
- `bottlecap/tests/apm_integration_test.rs`: `new_error_sampler(DEFAULT_APM_ERROR_TPS)`.

## New Files
None.

## Final Verification
Run everything from `bottlecap/` and report the pass/fail counts.
1. `cargo fmt --all -- --check`
2. `cargo test --workspace`. Use `cargo nextest run --workspace` if nextest is installed, to match CI.
3. `cargo nextest run --features test-mode --bin bottlecap-test-mode` (or `cargo test --features test-mode --bin bottlecap-test-mode`).
4. `cargo clippy --workspace --all-targets --features default -- -D warnings`
5. `cargo clippy --workspace --all-targets --features default,test-mode -- -D warnings`
6. `cargo clippy --workspace --all-targets --no-default-features --features fips -- -D warnings`. CI runs this. If the FIPS toolchain (aws-lc-fips-sys build deps) is missing locally, say it was skipped and why.
7. Run `ffgrep` for `AlwaysKeep` in `bottlecap/src`: zero matches outside comments. Run `ffgrep` for `not supported by the Lambda extension`: zero matches.
8. Grep the changed lines for `—` (em dash): zero matches.

## Commits and PR (only when the user asks)
- Commit 1 (step 1 only): `docs(bottlecap): DD_APM_FEATURES accepts space-separated values`
- Commit 2 (steps 2-4): `feat(bottlecap): rate-limit error trace rescue with DD_APM_ERROR_TPS`. Body:
  - Rescue is on by default at 10 errored traces/s per execution environment when the extension computes stats.
  - `DD_APM_ERROR_TPS<=0` disables it.
  - `DD_SERVERLESS_ERROR_SAMPLER_ENABLED` is deprecated and logs a warning.
- PR title: `feat(bottlecap): rate-limit error trace rescue with DD_APM_ERROR_TPS` (no ticket).
- `## Overview`:
  - Follow-up to APMSVLS-469 (the bottlecap error sampler) and part 1 of 3; name PRs 2 and 3 briefly.
  - The semantics table.
  - Env parity with the Go trace-agent's `DD_APM_ERROR_TPS`; YAML key is flat `apm_error_tps`.
  - The behavior change: compute-stats users who never set the flag now get rescue at 10/s.
  - The caveats: per-environment budget, compute-stats path only, approximate limit with no hard ceiling, inherited from Go/SCL.
  - `_dd.errors_sr` can now be below 1.0 under load.
- `## Testing`: describe what is covered. Do not include test counts.

## Risks
- **The settled caveat wording conflicts with the code.** The caveat says "unknown signatures sample at 1.0 during the first 30s". That is not exactly what happens. `Sampler::default_rate` (`score_sampler.rs`, `fn default_rate`) returns `min(1, target_tps / (max_bucket_count / 5))`, counted after the current trace, so unknown signatures are throttled even in the first bucket once volume passes 5x TPS. "No hard ceiling" is still true: each decision is a trace-id hash, and keeps grow roughly logarithmically within a bucket. Use the wording from step 2.4 and point this out to the user rather than repeating the original phrasing.
- **YAML key divergence (settled).** The Go agent's YAML key is believed to be `apm_config.errors_per_second` (inferred, not verified). Bottlecap uses flat `apm_error_tps` by decision: parity target is env vars, not YAML. State it in one line in the PR.
- **Flaky-test risk.** The shared-budget test uses the real wall clock through `process_traces`, and a 5s bucket boundary can fall mid-test. Comparing against a reset control (`shared * 2 < control`) tolerates that, because rotation only throttles the shared run further. Do not assert exact counts.
- **Throttled recovery after freeze/thaw (inherited).** After a long gap, stale counters are cleared but a throttled signature's stored rate only rises 1.2x per evaluation, and it also holds `lowest_rate` (the default for unknown signatures) down. A thawed sandbox can therefore under-rescue for a while after a past burst. Same as the Go agent and SCL; document it in the `apm_error_tps` doc and the PR, do not deviate.
- **Legacy-only YAML users.** A user with `serverless_error_sampler_enabled: true` and no TPS keeps the same on/off result, but rescue is now rate-limited instead of keep-all. This is intended; state it in the PR.
- **Duplicate warnings.** The non-finite warning in the deserializer can fire once per source (YAML and env). That is acceptable at startup.
- **Lints.** Clippy pedantic needs `#[must_use]` on the new pub fns and backticks around identifiers in docs (`doc_markdown`). Use `.expect("...")`, not `.unwrap()`, in new tests unless the module already has `#[allow(clippy::unwrap_used)]` (the config test module does; the trace_processor test module relies on `expect`).

## Follow-up PRs (out of scope for this plan; plan each in its own session)

### PR 2: rescue when the tracer computes stats
- Base: PR 1's branch (or `main` after PR 1 merges).
- Run the error sampler when either the extension or the tracer computes stats (resolve stats ownership once in `process_traces`, as #1358 does today).
- Tracer-owned path: do not drop any chunk. For errored P0 chunks (plus negative-priority non-manual drops under `error_rare_sample_tracer_drop`), call the sampler and stamp `_dd.errors_sr` on `Keep`; forward everything. The backend's P0 drop then keeps the stamped chunks.
- Extension-owned path: unchanged from PR 1 (rescue + drop).
- Update the `apm_error_tps` doc ("rescue runs when the extension or the tracer computes stats").
- Source material: the rescue-widening half of #1358 (`lpimentel/filter-sampled-traces-by-stats-owner`).

### PR 3: filter sampled-out chunks when the tracer computes stats
- Rebase DataDog/datadog-lambda-extension#1358 onto PR 2 so it only adds dropping of sampled-out chunks on the tracer-owned path (rescued chunks kept). Keep its review history; update its title and description.
