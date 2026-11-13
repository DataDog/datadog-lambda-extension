#!/bin/bash
# Downloads the recordings bottlecap uploaded.
set -euo pipefail
source "$(dirname "$0")/config.sh"
rm -rf "$HERE/out/recording" && mkdir -p "$HERE/out/recording"
aws s3 cp --recursive "s3://$BUCKET/recordings/$FUNCTION/" "$HERE/out/recording/"
find "$HERE/out/recording" -name '*.jsonl' | sort
