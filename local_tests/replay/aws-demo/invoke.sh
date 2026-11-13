#!/bin/bash
# Invokes the deployed demo once per event in events.json.
set -euo pipefail
source "$(dirname "$0")/config.sh"
python3 -c 'import json,sys; [print(json.dumps(e)) for e in json.load(open(sys.argv[1]))]' "$HERE/events.json" |
while read -r event; do
  aws lambda invoke --function-name "$FUNCTION" --cli-binary-format raw-in-base64-out \
    --payload "$event" /dev/stdout --query StatusCode --output text
  echo
done
