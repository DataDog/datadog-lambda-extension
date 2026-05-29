#!/bin/bash
set -e

# Reusable script to build Ruby Lambda functions.
# For simple Ruby Lambdas with no gem dependencies, this just packages the
# source as-is — the runtime + Datadog tracer layer provide everything needed.
# If the function gains a Gemfile, this script grows a bundle install step
# in a Docker container (mirroring build-python.sh / build-node.sh).
#
# Usage:
#   ./build-ruby.sh                    # Build all Ruby Lambda functions
#   ./build-ruby.sh <path-to-lambda>   # Build a specific Lambda function

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAMBDA_BASE_DIR="$SCRIPT_DIR/../lambda"

build_ruby_lambda() {
    local LAMBDA_DIR="$1"
    local FUNCTION_NAME=$(basename "$LAMBDA_DIR")

    if [ ! -d "$LAMBDA_DIR" ]; then
        echo "Error: Directory not found: $LAMBDA_DIR"
        return 1
    fi

    echo "Building Ruby Lambda: $FUNCTION_NAME"

    if [ ! -f "$LAMBDA_DIR/Gemfile" ]; then
        echo "ℹ No Gemfile found — source files are deployed as-is"
        return 0
    fi

    echo "Error: Gemfile-based Ruby builds are not implemented yet" >&2
    echo "  Add a Dockerised \`bundle install\` step to this script when needed." >&2
    return 1
}

if [ -z "$1" ]; then
    echo "=========================================="
    echo "Building all Ruby Lambda functions"
    echo "=========================================="
    echo ""

    FOUND_RUBY=0
    FAILED_BUILDS=()

    for LAMBDA_PATH in "$LAMBDA_BASE_DIR"/*; do
        if [ ! -d "$LAMBDA_PATH" ]; then
            continue
        fi

        FUNCTION_NAME=$(basename "$LAMBDA_PATH")

        if [[ "$FUNCTION_NAME" == *"ruby"* ]]; then
            FOUND_RUBY=1
            echo "----------------------------------------"
            if build_ruby_lambda "$LAMBDA_PATH"; then
                echo "✓ $FUNCTION_NAME built successfully"
            else
                echo "✗ $FUNCTION_NAME failed"
                FAILED_BUILDS+=("$FUNCTION_NAME")
            fi
            echo ""
        fi
    done

    if [ $FOUND_RUBY -eq 0 ]; then
        echo "No Ruby Lambda functions found (looking for directories with 'ruby' in name)"
        exit 0
    fi

    if [ ${#FAILED_BUILDS[@]} -eq 0 ]; then
        echo "✓ All Ruby Lambda builds completed successfully!"
        exit 0
    fi

    echo "✗ ${#FAILED_BUILDS[@]} Ruby Lambda build(s) failed:"
    for failed in "${FAILED_BUILDS[@]}"; do
        echo "  - $failed"
    done
    exit 1
else
    LAMBDA_DIR="$1"
    if [[ "$LAMBDA_DIR" != /* ]]; then
        LAMBDA_DIR="$(cd "$SCRIPT_DIR/.." && pwd)/$LAMBDA_DIR"
    fi
    build_ruby_lambda "$LAMBDA_DIR"
fi
