#!/bin/bash
set -e

# Reusable script to build Node.js Lambda functions
# Usage:
#   ./build-node.sh                    # Build all Node.js Lambda functions
#   ./build-node.sh <path-to-lambda>   # Build specific Lambda function
# Example: ./build-node.sh lambda/otlp-node

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAMBDA_BASE_DIR="$SCRIPT_DIR/../lambda"

# Function to build a single Node.js Lambda
build_node_lambda() {
    local LAMBDA_DIR="$1"
    local FUNCTION_NAME=$(basename "$LAMBDA_DIR")

    if [ ! -d "$LAMBDA_DIR" ]; then
        echo "Error: Directory not found: $LAMBDA_DIR"
        return 1
    fi

    echo "Building Node.js Lambda: $FUNCTION_NAME"

    # Check if package.json exists
    if [ ! -f "$LAMBDA_DIR/package.json" ]; then
        echo "ℹ No package.json found - skipping build (no dependencies)"
        return 0
    fi

    # Check if package.json has dependencies
    if ! grep -q '"dependencies"' "$LAMBDA_DIR/package.json"; then
        echo "ℹ No dependencies in package.json - skipping build"
        return 0
    fi

    # Check if dependencies object is empty
    if grep -A1 '"dependencies"' "$LAMBDA_DIR/package.json" | grep -q '{}'; then
        echo "ℹ Empty dependencies in package.json - skipping build"
        return 0
    fi

    echo "Found dependencies in package.json - installing modules"

    # Check if Docker is available
    if ! command -v docker &> /dev/null; then
        echo "Error: Docker is not installed or not in PATH"
        echo "Please install Docker: https://docs.docker.com/get-docker/"
        return 1
    fi

    # Clean previous build (idempotent)
    rm -rf "$LAMBDA_DIR/node_modules"

    # Determine npm cache directory
    if [ -n "$CI" ]; then
        # In CI: use local cache directory
        NPM_CACHE_DIR="$SCRIPT_DIR/../.cache/npm"
        mkdir -p "$NPM_CACHE_DIR"
    else
        # Local development: use host's npm cache
        NPM_CACHE_DIR="$HOME/.npm"
    fi

    # Determine Node image (use AWS ECR Public to avoid Docker Hub rate limits)
    NODE_IMAGE="public.ecr.aws/docker/library/node:20-alpine"

    # Install dependencies with Docker using ARM64 platform
    # Mount npm cache for faster package downloads
    docker run --rm --platform linux/arm64 \
      -v "$LAMBDA_DIR":/workspace \
      -v "$NPM_CACHE_DIR":/root/.npm \
      -w /workspace \
      "$NODE_IMAGE" \
      npm ci --production

    if [ -d "$LAMBDA_DIR/node_modules" ] && [ "$(ls -A "$LAMBDA_DIR/node_modules")" ]; then
        echo "✓ Build complete: $LAMBDA_DIR/node_modules/"
        echo "Installed packages:"
        ls -d "$LAMBDA_DIR/node_modules"/*/ | head -10
        return 0
    else
        echo "✗ Build failed: node_modules/ directory is empty"
        return 1
    fi
}

# Main logic: build all or build one
if [ -z "$1" ]; then
    # No argument: build all Node.js Lambda functions
    echo "=========================================="
    echo "Building all Node.js Lambda functions"
    echo "=========================================="
    echo ""

    FOUND_NODE=0
    BUILT_COUNT=0
    SKIPPED_COUNT=0
    FAILED_BUILDS=()

    for LAMBDA_PATH in "$LAMBDA_BASE_DIR"/*; do
        if [ ! -d "$LAMBDA_PATH" ]; then
            continue
        fi

        FUNCTION_NAME=$(basename "$LAMBDA_PATH")

        # Check if this is a Node.js function (contains "node" in name)
        if [[ "$FUNCTION_NAME" == *"node"* ]]; then
            FOUND_NODE=1
            echo "----------------------------------------"
            if build_node_lambda "$LAMBDA_PATH"; then
                # Check if it was actually built or skipped
                if [ -d "$LAMBDA_PATH/node_modules" ]; then
                    echo "✓ $FUNCTION_NAME built successfully"
                    BUILT_COUNT=$((BUILT_COUNT + 1))
                else
                    echo "ℹ $FUNCTION_NAME skipped (no dependencies)"
                    SKIPPED_COUNT=$((SKIPPED_COUNT + 1))
                fi
            else
                echo "✗ $FUNCTION_NAME failed"
                FAILED_BUILDS+=("$FUNCTION_NAME")
            fi
            echo ""
        fi
    done

    if [ $FOUND_NODE -eq 0 ]; then
        echo "No Node.js Lambda functions found (looking for directories with 'node' in name)"
        exit 1
    fi

    # Summary
    echo "Built: $BUILT_COUNT, Skipped: $SKIPPED_COUNT"
    if [ ${#FAILED_BUILDS[@]} -eq 0 ]; then
        echo "✓ All Node.js Lambda builds completed successfully!"
        exit 0
    else
        echo "✗ ${#FAILED_BUILDS[@]} Node.js Lambda build(s) failed:"
        for failed in "${FAILED_BUILDS[@]}"; do
            echo "  - $failed"
        done
        exit 1
    fi
else
    # Argument provided: build specific Lambda function
    LAMBDA_DIR="$1"

    # Convert to absolute path if relative
    if [[ "$LAMBDA_DIR" != /* ]]; then
        LAMBDA_DIR="$(cd "$SCRIPT_DIR/.." && pwd)/$LAMBDA_DIR"
    fi

    build_node_lambda "$LAMBDA_DIR"
fi
