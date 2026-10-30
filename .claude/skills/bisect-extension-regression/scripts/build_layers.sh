#!/bin/bash
# Build and publish one layer for each commit, in order, in a separate worktree.
# Usage: [ARCHITECTURE=amd64] [REGION=us-east-1] [FIPS=1] build_layers.sh <worktree> <layer-suffix> <commit>...
# The defaults are ARCHITECTURE=arm64, REGION=us-east-2 and FIPS=0.
# Appends "<commit> <layer version ARN>" to <worktree>/layers.txt.
set -uo pipefail
WORKTREE="$(cd "$1" && pwd)" || exit 1; SUFFIX="$2"; shift 2
ARCHITECTURE="${ARCHITECTURE:-arm64}"; REGION="${REGION:-us-east-2}"; FIPS="${FIPS:-0}"
ZIP=".layers/datadog_extension-$ARCHITECTURE.zip"
[ "$ARCHITECTURE" = arm64 ] && LAYER="Datadog-Bottlecap-Beta-ARM-$SUFFIX" || LAYER="Datadog-Bottlecap-Beta-$SUFFIX"
cd "$WORKTREE" || exit 1
for c in "$@"; do
  git checkout -q --detach "$c" || exit 1
  log="$WORKTREE/build-$c.log"
  start=$(date +%s)
  # build_bottlecap_layer.sh was seen to exit 0 after a failed step, and it keeps an earlier zip.
  # Without the earlier zip, a failed build cannot publish the layer of another commit.
  rm -f "$ZIP"
  ARCHITECTURE="$ARCHITECTURE" REGION="$REGION" SUFFIX="$SUFFIX" FIPS="$FIPS" ./scripts/publish_bottlecap_sandbox.sh > "$log" 2>&1
  version=$(sed -n 's/^DONE: Published version \([0-9]*\) .*/\1/p' "$log")
  [ -n "$version" ] || { echo "$c: publish failed, see $log"; continue; }
  [ -f "$ZIP" ] || { echo "$c: the build made no zip, see $log"; continue; }
  arn="arn:aws:lambda:$REGION:425362996713:layer:$LAYER:$version"
  echo "$c $arn" | tee -a "$WORKTREE/layers.txt"
  echo "  built and published in $(( $(date +%s) - start )) s"
done
