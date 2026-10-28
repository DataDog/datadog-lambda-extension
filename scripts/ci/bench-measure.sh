#!/usr/bin/env bash
# Helper for .github/workflows/bench.yml (job bench-measure).
#
#   bench-measure.sh download-baseline <dir>   # latest nightly `bench-baseline-main` artifact (needs gh)
#   bench-measure.sh run                        # from bottlecap/; BENCH_MODE=smoke|full
#
# smoke: a short fixed list with `--quick`, exported as pr.json, compared with
#        <BENCH_BASELINE_DIR>/main.json when present (critcmp, 10 % threshold).
# full:  the whole suite, baseline "main", exported as main.json.
# Exports land in .bench-export/. With no [[bench]] target in the checkout the
# run is a no-op, so the job is green before the suite lands on main.
set -euo pipefail

export_dir=".bench-export"
# bottlecap/benches/README.md, "CI" item 2: one id per hot path.
smoke_list="logs:processor_process logs:oom_check traces:process_traces/default traces:decode_v04 invocation:trigger/from_value/sqs_event enhanced_metrics:set_max config:get_config/typical"

summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then tee -a "$GITHUB_STEP_SUMMARY"; else cat; fi
}

case "${1:-}" in
download-baseline)
  dir="${2:?usage: bench-measure.sh download-baseline <dir>}"
  mkdir -p "$dir"
  # A nightly whose measure step failed is still green but has no artifact, so walk back to
  # the newest run that has one.
  run_ids="$(gh run list --workflow bench.yml --branch main --event schedule --status success --limit 10 \
    --json databaseId --jq '.[].databaseId' || true)"
  for run_id in $run_ids; do
    if gh run download "$run_id" --name bench-baseline-main --dir "$dir"; then
      echo "baseline: run $run_id -> $dir/main.json"
      exit 0
    fi
    echo "run $run_id has no bench-baseline-main artifact; trying an older run"
  done
  echo "no bench-baseline-main artifact in the last 10 successful nightly runs on main; no baseline"
  ;;

run)
  mode="${BENCH_MODE:-smoke}"
  baseline_dir="${BENCH_BASELINE_DIR:-}"
  targets="$(cargo metadata --no-deps --format-version 1 --locked \
    | jq -r '.packages[].targets[] | select(.kind | index("bench")) | .name')"
  if [ -z "$targets" ]; then
    echo "no [[bench]] targets in this checkout; nothing to measure"
    exit 0
  fi
  mkdir -p "$export_dir"
  if [ "$mode" = "full" ]; then
    cargo bench --locked -- --save-baseline main
    critcmp --export main > "$export_dir/main.json"
    { echo "### Bench full run (baseline main)"; echo '```'; critcmp main; echo '```'; } | summary
    exit 0
  fi
  for pair in $smoke_list; do
    bench="${pair%%:*}"
    filter="${pair#*:}"
    if ! grep -qx "$bench" <<<"$targets"; then
      echo "skip $bench/$filter: no such bench target"
      continue
    fi
    cargo bench --locked --bench "$bench" -- --save-baseline pr --quick "$filter"
  done
  critcmp --export pr > "$export_dir/pr.json"
  if [ -n "$baseline_dir" ] && [ -f "$baseline_dir/main.json" ]; then
    cp "$baseline_dir/main.json" "$export_dir/main.json"
    {
      echo "### Bench smoke vs main (informational: shared runner, differences under 10 % hidden)"
      echo '```'
      critcmp "$export_dir/main.json" "$export_dir/pr.json" -t 10
      echo '```'
    } | summary
  else
    { echo "### Bench smoke (no main baseline yet)"; echo '```'; critcmp "$export_dir/pr.json"; echo '```'; } | summary
  fi
  ;;

*)
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
