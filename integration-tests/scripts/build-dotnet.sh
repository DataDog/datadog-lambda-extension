#!/bin/bash
set -e

# Reusable script to build .NET Lambda functions
# Usage:
#   ./build-dotnet.sh                    # Build all .NET Lambda functions
#   ./build-dotnet.sh <path-to-lambda>   # Build specific Lambda function
# Example: ./build-dotnet.sh lambda/otlp-dotnet

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAMBDA_BASE_DIR="$SCRIPT_DIR/../lambda"

# Function to build a single .NET Lambda
build_dotnet_lambda() {
    local LAMBDA_DIR="$1"
    local FUNCTION_NAME=$(basename "$LAMBDA_DIR")

    if [ ! -d "$LAMBDA_DIR" ]; then
        echo "Error: Directory not found: $LAMBDA_DIR"
        return 1
    fi

    # Check for .csproj file
    if ! ls "$LAMBDA_DIR"/*.csproj 1> /dev/null 2>&1; then
        echo "Error: No .csproj file found in $LAMBDA_DIR"
        echo "This script is for .NET projects only"
        return 1
    fi

    echo "Building .NET Lambda: $FUNCTION_NAME"

    # Check if Docker is available
    if ! command -v docker &> /dev/null; then
        echo "Error: Docker is not installed or not in PATH"
        echo "Please install Docker: https://docs.docker.com/get-docker/"
        return 1
    fi

    # Clean previous build (idempotent)
    rm -rf "$LAMBDA_DIR/bin" "$LAMBDA_DIR/obj"

    # Determine NuGet cache directory
    if [ -n "$CI" ]; then
        # In CI: use local cache directory
        NUGET_CACHE_DIR="$SCRIPT_DIR/../.cache/nuget"
        mkdir -p "$NUGET_CACHE_DIR"
    else
        # Local development: use host's NuGet cache
        NUGET_CACHE_DIR="$HOME/.nuget"
    fi

    # Build and package with Docker using ARM64 platform
    # Mount NuGet cache for faster package downloads
    docker run --rm --platform linux/arm64 \
      -v "$LAMBDA_DIR":/workspace \
      -v "$NUGET_CACHE_DIR":/root/.nuget \
      -w /workspace \
      mcr.microsoft.com/dotnet/sdk:8.0-alpine \
      sh -c "apk add --no-cache zip && \
             dotnet tool install -g Amazon.Lambda.Tools || true && \
             export PATH=\"\$PATH:/root/.dotnet/tools\" && \
             dotnet lambda package -o bin/function.zip --function-architecture arm64"

    if [ -f "$LAMBDA_DIR/bin/function.zip" ]; then
        echo "✓ Build complete: $LAMBDA_DIR/bin/function.zip"
        ls -lh "$LAMBDA_DIR/bin/function.zip"
        return 0
    else
        echo "✗ Build failed: bin/function.zip not found"
        return 1
    fi
}

# Main logic: build all or build one
if [ -z "$1" ]; then
    # No argument: build all .NET Lambda functions
    echo "=========================================="
    echo "Building all .NET Lambda functions"
    echo "=========================================="
    echo ""

    FOUND_DOTNET=0
    FAILED_BUILDS=()

    for LAMBDA_PATH in "$LAMBDA_BASE_DIR"/*; do
        if [ ! -d "$LAMBDA_PATH" ]; then
            continue
        fi

        FUNCTION_NAME=$(basename "$LAMBDA_PATH")

        # Check if this is a .NET function (contains "dotnet" in name)
        if [[ "$FUNCTION_NAME" == *"dotnet"* ]]; then
            FOUND_DOTNET=1
            echo "----------------------------------------"
            if build_dotnet_lambda "$LAMBDA_PATH"; then
                echo "✓ $FUNCTION_NAME succeeded"
            else
                echo "✗ $FUNCTION_NAME failed"
                FAILED_BUILDS+=("$FUNCTION_NAME")
            fi
            echo ""
        fi
    done

    if [ $FOUND_DOTNET -eq 0 ]; then
        echo "No .NET Lambda functions found (looking for directories with 'dotnet' in name)"
        exit 1
    fi

    # Summary
    if [ ${#FAILED_BUILDS[@]} -eq 0 ]; then
        echo "✓ All .NET Lambda builds completed successfully!"
        exit 0
    else
        echo "✗ ${#FAILED_BUILDS[@]} .NET Lambda build(s) failed:"
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

    build_dotnet_lambda "$LAMBDA_DIR"
fi
