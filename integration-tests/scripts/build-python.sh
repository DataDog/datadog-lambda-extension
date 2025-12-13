#!/bin/bash
set -e

# Reusable script to build Python Lambda functions
# Usage:
#   ./build-python.sh                    # Build all Python Lambda functions
#   ./build-python.sh <path-to-lambda>   # Build specific Lambda function
# Example: ./build-python.sh lambda/otlp-python

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAMBDA_BASE_DIR="$SCRIPT_DIR/../lambda"

# Function to build a single Python Lambda
build_python_lambda() {
    local LAMBDA_DIR="$1"
    local FUNCTION_NAME=$(basename "$LAMBDA_DIR")

    if [ ! -d "$LAMBDA_DIR" ]; then
        echo "Error: Directory not found: $LAMBDA_DIR"
        return 1
    fi

    echo "Building Python Lambda: $FUNCTION_NAME"

    # Check if requirements.txt exists and has actual dependencies
    if [ ! -f "$LAMBDA_DIR/requirements.txt" ]; then
        echo "ℹ No requirements.txt found - skipping build (no dependencies)"
        return 0
    fi

    # Check if requirements.txt has any non-comment, non-empty lines
    if ! grep -v '^#' "$LAMBDA_DIR/requirements.txt" | grep -v '^$' | grep -q .; then
        echo "ℹ requirements.txt is empty or has only comments - skipping build"
        return 0
    fi

    echo "Found dependencies in requirements.txt - building package"

    # Check if Docker is available
    if ! command -v docker &> /dev/null; then
        echo "Error: Docker is not installed or not in PATH"
        echo "Please install Docker: https://docs.docker.com/get-docker/"
        return 1
    fi

    # Clean previous build (idempotent)
    rm -rf "$LAMBDA_DIR/package"
    mkdir -p "$LAMBDA_DIR/package"

    # Determine pip cache directory
    if [ -n "$CI" ]; then
        # In CI: use local cache directory
        PIP_CACHE_DIR="$SCRIPT_DIR/../.cache/pip"
        mkdir -p "$PIP_CACHE_DIR"
    else
        # Local development: use host's pip cache
        PIP_CACHE_DIR="$HOME/.cache/pip"
    fi

    # Install dependencies with Docker using ARM64 platform
    # Use the same image that CDK would use for consistency
    # Mount pip cache for faster package downloads
    docker run --rm --platform linux/arm64 \
      -v "$LAMBDA_DIR":/workspace \
      -v "$PIP_CACHE_DIR":/root/.cache/pip \
      -w /workspace \
      public.ecr.aws/sam/build-python3.12 \
      pip install -r requirements.txt -t package/

    # Copy source files to package directory
    cp -r "$LAMBDA_DIR"/*.py "$LAMBDA_DIR/package/" 2>/dev/null || true

    if [ -d "$LAMBDA_DIR/package" ] && [ "$(ls -A "$LAMBDA_DIR/package")" ]; then
        echo "✓ Build complete: $LAMBDA_DIR/package/"
        echo "Package contents:"
        ls -lh "$LAMBDA_DIR/package" | head -10
        return 0
    else
        echo "✗ Build failed: package/ directory is empty"
        return 1
    fi
}

# Main logic: build all or build one
if [ -z "$1" ]; then
    # No argument: build all Python Lambda functions
    echo "=========================================="
    echo "Building all Python Lambda functions"
    echo "=========================================="
    echo ""

    FOUND_PYTHON=0
    BUILT_COUNT=0
    SKIPPED_COUNT=0
    FAILED_BUILDS=()

    for LAMBDA_PATH in "$LAMBDA_BASE_DIR"/*; do
        if [ ! -d "$LAMBDA_PATH" ]; then
            continue
        fi

        FUNCTION_NAME=$(basename "$LAMBDA_PATH")

        # Check if this is a Python function (contains "python" in name)
        if [[ "$FUNCTION_NAME" == *"python"* ]]; then
            FOUND_PYTHON=1
            echo "----------------------------------------"
            if build_python_lambda "$LAMBDA_PATH"; then
                # Check if it was actually built or skipped
                if [ -d "$LAMBDA_PATH/package" ]; then
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

    if [ $FOUND_PYTHON -eq 0 ]; then
        echo "No Python Lambda functions found (looking for directories with 'python' in name)"
        exit 1
    fi

    # Summary
    echo "Built: $BUILT_COUNT, Skipped: $SKIPPED_COUNT"
    if [ ${#FAILED_BUILDS[@]} -eq 0 ]; then
        echo "✓ All Python Lambda builds completed successfully!"
        exit 0
    else
        echo "✗ ${#FAILED_BUILDS[@]} Python Lambda build(s) failed:"
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

    build_python_lambda "$LAMBDA_DIR"
fi
