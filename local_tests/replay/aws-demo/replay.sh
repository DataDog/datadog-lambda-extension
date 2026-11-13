#!/bin/bash
# Replays the fetched recording offline on this machine, with boto3 from a
# local venv. Extra arguments go to lambda-replay (e.g. --env CHANGED_BEHAVIOR=1).
set -uo pipefail
source "$(dirname "$0")/config.sh"
BIN="$ROOT/bottlecap/target/debug"
VENV="$HERE/out/venv"
[ -x "$VENV/bin/python" ] || { python3 -m venv "$VENV" && "$VENV/bin/pip" -q install boto3; }
unset AWS_PROFILE
"$BIN/lambda-replay" replay --recording "$HERE/out/recording" --report "$HERE/out/report.json" \
  --extension "$BIN/bottlecap" --handler handler.handler \
  --env "LAMBDA_TASK_ROOT=$HERE/function" --env "PYTHONPATH=$HERE/function" "$@" \
  -- "$VENV/bin/python" "$HERE/../bootstrap"
