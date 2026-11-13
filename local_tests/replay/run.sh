#!/bin/bash
# The dev-loop workflow against a local upstream, end to end:
#   record -> replay (PASS) -> replay without rules (FAIL) -> code change (FAIL)
#   -> --update records only the new call -> replay the change (PASS)
# Needs `cargo build` in bottlecap/ first. VERBOSE=1 shows extension logs.
set -uo pipefail
cd "$(dirname "$0")"
ROOT="$(cd ../.. && pwd)"
BIN="$ROOT/bottlecap/target/debug"
HERE="$(pwd)"
FLAGS=(--extension "$BIN/bottlecap" --handler handler.handler --env "LAMBDA_TASK_ROOT=$HERE/function")
[ -n "${VERBOSE:-}" ] && FLAGS+=(--verbose)
REC=out/recording

start_upstream() {
  python3 -m http.server 8088 --bind ::1 --directory upstream >/tmp/lambda-replay-upstream.log 2>&1 &
  UPSTREAM=$!; sleep 0.5
}
stop_upstream() { kill $UPSTREAM 2>/dev/null; wait $UPSTREAM 2>/dev/null; }
replay() { "$BIN/lambda-replay" replay --recording $REC "${FLAGS[@]}" "$@" -- "$HERE/bootstrap"; echo "exit code: $?"; }
trap stop_upstream EXIT

rm -rf out && mkdir -p out
# The layer build makes the wrapper executable; the checkout does not.
install -m 755 "$ROOT/scripts/datadog_wrapper" out/datadog_wrapper

echo "=== 1. record against the live upstream ==="
start_upstream
"$BIN/lambda-replay" record --events events.json --out $REC --wrapper "$HERE/out/datadog_wrapper" \
  --env "UPSTREAM_URL=http://[::1]:8088" "${FLAGS[@]}" -- "$HERE/bootstrap" || exit 1
cp rules.json $REC/rules.json
stop_upstream   # replay must not need the network

echo; echo "=== 2. replay, unchanged (expect PASS) ==="
replay
echo; echo "=== 3. replay without rules.json (expect FAIL: nonce and timestamp differ) ==="
replay --rules /nonexistent 2>&1 | grep -E "FAIL|PASS|unexpected|generated_at|exit code" | head -6
echo; echo "=== 4. replay a code change (expect FAIL) ==="
replay --env CHANGED_BEHAVIOR=1 2>&1 | grep -E "FAIL|PASS|unexpected|exit code"

echo; echo "=== 5. --update: accept the change, recording only the new call ==="
start_upstream
replay --update --env CHANGED_BEHAVIOR=1
stop_upstream

echo; echo "=== 6. replay the change against the updated recording (expect PASS) ==="
replay --env CHANGED_BEHAVIOR=1
