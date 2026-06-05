#!/bin/bash
set -e

# Reusable script to cross-compile Go Lambda functions for ARM64 Linux.
# Outputs a binary named `bootstrap` (required by the AWS Lambda custom runtime
# provided.al2023) under <lambda-dir>/bin/.
#
# Usage:
#   ./build-go.sh                    # Build all Go Lambda functions
#   ./build-go.sh <path-to-lambda>   # Build a specific Lambda function

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAMBDA_BASE_DIR="$SCRIPT_DIR/../lambda"

build_go_lambda() {
    local LAMBDA_DIR="$1"
    local FUNCTION_NAME=$(basename "$LAMBDA_DIR")

    if [ ! -d "$LAMBDA_DIR" ]; then
        echo "Error: Directory not found: $LAMBDA_DIR"
        return 1
    fi

    if [ ! -f "$LAMBDA_DIR/go.mod" ]; then
        echo "Error: go.mod not found in $LAMBDA_DIR"
        return 1
    fi

    echo "Building Go Lambda: $FUNCTION_NAME"

    if ! command -v docker &> /dev/null; then
        echo "Error: Docker is not installed or not in PATH"
        return 1
    fi

    # Clean previous build (idempotent).
    rm -rf "$LAMBDA_DIR/bin"
    mkdir -p "$LAMBDA_DIR/bin"

    # Module cache: reuse the host's $GOPATH/pkg/mod when running locally;
    # use a project-local cache in CI so it can be cached between jobs.
    if [ -n "$CI" ]; then
        GO_MOD_CACHE="$SCRIPT_DIR/../.cache/go-mod"
        mkdir -p "$GO_MOD_CACHE"
    else
        GO_MOD_CACHE="${GOPATH:-$HOME/go}/pkg/mod"
        mkdir -p "$GO_MOD_CACHE"
    fi

    # Cross-compile to ARM64 Linux inside the official Go image.
    # CGO is disabled so the binary runs on the provided.al2023 base image
    # without a libc mismatch.
    docker run --rm --platform linux/arm64 \
        -v "$LAMBDA_DIR":/workspace \
        -v "$GO_MOD_CACHE":/go/pkg/mod \
        -w /workspace \
        -e GOOS=linux \
        -e GOARCH=arm64 \
        -e CGO_ENABLED=0 \
        public.ecr.aws/docker/library/golang:1.22-bookworm \
        sh -c "go mod tidy && go build -o bin/bootstrap ."

    if [ ! -f "$LAMBDA_DIR/bin/bootstrap" ]; then
        echo "✗ Build failed: bin/bootstrap not produced"
        return 1
    fi

    echo "✓ Build complete: $LAMBDA_DIR/bin/bootstrap"
    return 0
}

if [ -z "$1" ]; then
    echo "=========================================="
    echo "Building all Go Lambda functions"
    echo "=========================================="
    echo ""

    FOUND_GO=0
    FAILED_BUILDS=()

    for LAMBDA_PATH in "$LAMBDA_BASE_DIR"/*; do
        if [ ! -d "$LAMBDA_PATH" ]; then
            continue
        fi

        FUNCTION_NAME=$(basename "$LAMBDA_PATH")

        # Match directories whose suffix is `-go` or whose name is exactly `go`.
        if [[ "$FUNCTION_NAME" == *"-go" || "$FUNCTION_NAME" == "go" ]]; then
            FOUND_GO=1
            echo "----------------------------------------"
            if build_go_lambda "$LAMBDA_PATH"; then
                echo "✓ $FUNCTION_NAME built successfully"
            else
                echo "✗ $FUNCTION_NAME failed"
                FAILED_BUILDS+=("$FUNCTION_NAME")
            fi
            echo ""
        fi
    done

    if [ $FOUND_GO -eq 0 ]; then
        echo "No Go Lambda functions found (looking for directories ending in -go)"
        exit 0
    fi

    if [ ${#FAILED_BUILDS[@]} -eq 0 ]; then
        echo "✓ All Go Lambda builds completed successfully!"
        exit 0
    fi

    echo "✗ ${#FAILED_BUILDS[@]} Go Lambda build(s) failed:"
    for failed in "${FAILED_BUILDS[@]}"; do
        echo "  - $failed"
    done
    exit 1
else
    LAMBDA_DIR="$1"
    if [[ "$LAMBDA_DIR" != /* ]]; then
        LAMBDA_DIR="$(cd "$SCRIPT_DIR/.." && pwd)/$LAMBDA_DIR"
    fi
    build_go_lambda "$LAMBDA_DIR"
fi
