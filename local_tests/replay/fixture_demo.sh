#!/bin/bash
# Recording-as-fixture workflow with pytest:
#   record once -> pytest offline (new test fails) -> LAMBDA_REPLAY_UPDATE=1 pytest
#   records only the new case -> pytest offline (all pass)
set -uo pipefail
cd "$(dirname "$0")"
ROOT="$(cd ../.. && pwd)"
BIN="$ROOT/bottlecap/target/debug"
HERE="$(pwd)"
REC=tests/recordings
export LAMBDA_REPLAY_BIN="$BIN/lambda-replay"
[ -x .venv/bin/pytest ] || { python3 -m venv .venv && .venv/bin/pip -q install pytest boto3; }

start_upstream() {
  python3 -m http.server 8088 --bind ::1 --directory upstream >/tmp/lambda-replay-upstream.log 2>&1 &
  UPSTREAM=$!; sleep 0.5
}
stop_upstream() { kill $UPSTREAM 2>/dev/null; wait $UPSTREAM 2>/dev/null; }
trap stop_upstream EXIT
mkdir -p out && install -m 755 "$ROOT/scripts/datadog_wrapper" out/datadog_wrapper

if [ ! -d $REC ] || [ -n "${RERECORD:-}" ]; then
  echo "=== record the fixture (two invocations) ==="
  rm -rf $REC && start_upstream
  echo '[{"id": "1"}, {"id": "2"}]' > out/fixture-events.json
  "$BIN/lambda-replay" record --events out/fixture-events.json --out $REC --wrapper "$HERE/out/datadog_wrapper" \
    --extension "$BIN/bottlecap" --handler handler.handler \
    --env "LAMBDA_TASK_ROOT=$HERE/function" --env "UPSTREAM_URL=http://[::1]:8088" -- "$HERE/bootstrap" || exit 1
  cp rules.json $REC/rules.json
  stop_upstream
fi

echo; echo "=== pytest, offline (expect test_new_item to fail) ==="
.venv/bin/pytest -q tests 2>&1 | grep -vE "^\s*$" | tail -15

echo; echo "=== LAMBDA_REPLAY_UPDATE=1 pytest: record only the new case ==="
start_upstream
LAMBDA_REPLAY_UPDATE=1 .venv/bin/pytest -q -s tests 2>&1 | grep -E "passed|failed|updated"
stop_upstream

echo; echo "=== pytest, offline again (expect all to pass) ==="
.venv/bin/pytest -q tests 2>&1 | tail -2
