#!/bin/bash
# repro-large-payload.sh
# Reproduces GitHub issue #1041: extension errors on Lambda payloads > 2 MB.
#
# Strategy: POST the large payload directly to the extension's
# /lambda/start-invocation endpoint (port 8124), exactly as the DD Java agent
# does in production. The extension binds to 127.0.0.1:8124 (loopback only),
# so we write the payload to a file, docker-cp it into the container, and
# send the request from inside the container via docker exec.
#
# Run from the repo root:
#   bash local_tests/repro-large-payload.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IMAGE_NAME="dd-extension-large-payload-repro"
CONTAINER_ID=""
LOG_FILE="$SCRIPT_DIR/large-payload-repro.log"
PAYLOAD_FILE=$(mktemp /tmp/large-payload-XXXXXX.json)

# 3 MB — above the old 2 MB axum default, below the new 6 MB limit.
PAYLOAD_CHARS=3200000

cleanup() {
    rm -f "$PAYLOAD_FILE"
    if [[ -n "$CONTAINER_ID" ]]; then
        docker logs "$CONTAINER_ID" > "$LOG_FILE" 2>&1 || true
        docker stop "$CONTAINER_ID" > /dev/null 2>&1 || true
        docker rm   "$CONTAINER_ID" > /dev/null 2>&1 || true
    fi
    docker rmi "$IMAGE_NAME" > /dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

# Always rebuild the Linux x86_64 binary from the current source.
# Mirrors the official AL2 build environment (images/Dockerfile.bottlecap.compile).
echo "==> Building Linux extension binary (~10-20 min first run, cached after)..."
rm -f "$SCRIPT_DIR/datadog-agent"
docker build \
    --platform linux/amd64 \
    -f "$SCRIPT_DIR/Dockerfile.build-bottlecap" \
    -t dd-bottlecap-builder \
    "$REPO_ROOT"
cid=$(docker create dd-bottlecap-builder)
docker cp "$cid:/bottlecap" "$SCRIPT_DIR/datadog-agent"
docker rm "$cid" > /dev/null
docker rmi dd-bottlecap-builder > /dev/null 2>&1 || true
chmod +x "$SCRIPT_DIR/datadog-agent"

echo "==> Building test image..."
docker build \
    --no-cache \
    --platform linux/amd64 \
    -f "$SCRIPT_DIR/Dockerfile.LargePayload" \
    -t "$IMAGE_NAME" \
    "$SCRIPT_DIR"

echo "==> Starting container..."
CONTAINER_ID=$(docker run -d --platform linux/amd64 "$IMAGE_NAME")

echo "==> Waiting for extension to bind port 8124..."
READY=false
for _ in $(seq 1 30); do
    if ! docker inspect "$CONTAINER_ID" --format='{{.State.Running}}' 2>/dev/null | grep -q "true"; then
        echo "ERROR: Container exited during init. Logs:"
        docker logs "$CONTAINER_ID" 2>&1 | tail -30
        exit 1
    fi
    if docker exec "$CONTAINER_ID" \
        curl -sf -o /dev/null \
        -X POST "http://localhost:8124/lambda/start-invocation" \
        -H "Content-Type: application/json" \
        -d '{}' --max-time 2 2>/dev/null; then
        READY=true
        break
    fi
    sleep 1
done

if [[ "$READY" != "true" ]]; then
    echo "ERROR: Extension did not become ready after 30s. Logs:"
    docker logs "$CONTAINER_ID" 2>&1
    exit 1
fi

echo "==> Sending ~3 MB payload to /lambda/start-invocation..."
python3 -c "
import json
payload = {'description': 'Large payload repro for GitHub issue #1041', 'data': 'x' * $PAYLOAD_CHARS}
print(json.dumps(payload))
" > "$PAYLOAD_FILE"

PAYLOAD_SIZE=$(wc -c < "$PAYLOAD_FILE")
docker cp "$PAYLOAD_FILE" "$CONTAINER_ID:/tmp/large-payload.json"

HTTP_CODE=$(docker exec "$CONTAINER_ID" \
    curl -s -o /dev/null -w "%{http_code}" \
    -X POST "http://localhost:8124/lambda/start-invocation" \
    -H "Content-Type: application/json" \
    -H "lambda-runtime-aws-request-id: test-large-payload-request" \
    -H "datadog-meta-lang: java" \
    --data-binary "@/tmp/large-payload.json" \
    --max-time 15) || HTTP_CODE="error"
sleep 1

ERRORS=$(docker logs "$CONTAINER_ID" 2>&1 | grep -E "length limit|extract request body" || true)

echo ""
echo "────────────────────────────────────────────────────────────"
if [[ -n "$ERRORS" ]]; then
    echo "RESULT: BUG REPRODUCED (fix not applied or not working)"
    echo ""
    echo "$ERRORS"
else
    echo "RESULT: OK — no 'length limit exceeded' error (fix is working)"
    echo "        HTTP $HTTP_CODE returned for a ${PAYLOAD_SIZE}-byte payload"
fi
echo "────────────────────────────────────────────────────────────"
echo ""
echo "Full logs saved to: $LOG_FILE"
