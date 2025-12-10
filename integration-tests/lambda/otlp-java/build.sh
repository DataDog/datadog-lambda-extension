#!/bin/bash
set -e

echo "Building Java Lambda with Docker (ARM64)..."

# Check if Docker is available
if ! command -v docker &> /dev/null; then
    echo "Error: Docker is not installed or not in PATH"
    echo "Please install Docker: https://docs.docker.com/get-docker/"
    exit 1
fi

# Get the directory of this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Clean previous build
rm -rf "$SCRIPT_DIR/target"

# Build with Docker using ARM64 platform
docker run --rm --platform linux/arm64 \
  -v "$SCRIPT_DIR":/workspace \
  -w /workspace \
  maven:3.9-eclipse-temurin-21-alpine \
  mvn clean package

if [ -f "$SCRIPT_DIR/target/function.jar" ]; then
    echo "✓ Build complete: target/function.jar"
    ls -lh "$SCRIPT_DIR/target/function.jar"
else
    echo "✗ Build failed: target/function.jar not found"
    exit 1
fi
