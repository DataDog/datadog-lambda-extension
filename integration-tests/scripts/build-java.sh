#!/bin/bash
set -e

# Reusable script to build Java Lambda functions with Maven
# Usage:
#   ./build-java.sh                    # Build all Java Lambda functions
#   ./build-java.sh <path-to-lambda>   # Build specific Lambda function
# Example: ./build-java.sh lambda/otlp-java

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAMBDA_BASE_DIR="$SCRIPT_DIR/../lambda"

# Function to build a single Java Lambda
build_java_lambda() {
    local LAMBDA_DIR="$1"
    local FUNCTION_NAME=$(basename "$LAMBDA_DIR")

    if [ ! -d "$LAMBDA_DIR" ]; then
        echo "Error: Directory not found: $LAMBDA_DIR"
        return 1
    fi

    if [ ! -f "$LAMBDA_DIR/pom.xml" ]; then
        echo "Error: pom.xml not found in $LAMBDA_DIR"
        echo "This script is for Java Maven projects only"
        return 1
    fi

    echo "Building Java Lambda: $FUNCTION_NAME"

    # Check if Docker is available
    if ! command -v docker &> /dev/null; then
        echo "Error: Docker is not installed or not in PATH"
        echo "Please install Docker: https://docs.docker.com/get-docker/"
        return 1
    fi

    # Clean previous build (idempotent)
    rm -rf "$LAMBDA_DIR/target"

    # Determine Maven cache directory
    if [ -n "$CI" ]; then
        # In CI: use local cache directory
        MAVEN_CACHE_DIR="$SCRIPT_DIR/../.cache/maven"
        mkdir -p "$MAVEN_CACHE_DIR"
    else
        # Local development: use host's Maven cache
        MAVEN_CACHE_DIR="$HOME/.m2"
    fi

    # Determine Maven image (use AWS ECR Public to avoid Docker Hub rate limits)
    MAVEN_IMAGE="public.ecr.aws/docker/library/maven:3.9-eclipse-temurin-21-alpine"

    # Build with Docker using ARM64 platform
    # Mount Maven cache for faster dependency downloads
    docker run --rm --platform linux/arm64 \
      -v "$LAMBDA_DIR":/workspace \
      -v "$MAVEN_CACHE_DIR":/root/.m2 \
      -w /workspace \
      "$MAVEN_IMAGE" \
      mvn clean package

    if [ -f "$LAMBDA_DIR/target/function.jar" ]; then
        echo "✓ Build complete: $LAMBDA_DIR/target/function.jar"
        ls -lh "$LAMBDA_DIR/target/function.jar"
        return 0
    else
        echo "✗ Build failed: target/function.jar not found"
        return 1
    fi
}

# Main logic: build all or build one
if [ -z "$1" ]; then
    # No argument: build all Java Lambda functions
    echo "=========================================="
    echo "Building all Java Lambda functions"
    echo "=========================================="
    echo ""

    FOUND_JAVA=0
    FAILED_BUILDS=()

    for LAMBDA_PATH in "$LAMBDA_BASE_DIR"/*; do
        if [ ! -d "$LAMBDA_PATH" ]; then
            continue
        fi

        FUNCTION_NAME=$(basename "$LAMBDA_PATH")

        # Check if this is a Java function (contains "java" in name)
        if [[ "$FUNCTION_NAME" == *"java"* ]]; then
            FOUND_JAVA=1
            echo "----------------------------------------"
            if build_java_lambda "$LAMBDA_PATH"; then
                echo "✓ $FUNCTION_NAME succeeded"
            else
                echo "✗ $FUNCTION_NAME failed"
                FAILED_BUILDS+=("$FUNCTION_NAME")
            fi
            echo ""
        fi
    done

    if [ $FOUND_JAVA -eq 0 ]; then
        echo "No Java Lambda functions found (looking for directories with 'java' in name)"
        exit 1
    fi

    # Summary
    if [ ${#FAILED_BUILDS[@]} -eq 0 ]; then
        echo "✓ All Java Lambda builds completed successfully!"
        exit 0
    else
        echo "✗ ${#FAILED_BUILDS[@]} Java Lambda build(s) failed:"
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

    build_java_lambda "$LAMBDA_DIR"
fi
