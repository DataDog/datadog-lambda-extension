#!/bin/bash
set -e

echo "Building .NET Lambda with Docker (ARM64)..."

# Check if Docker is available
if ! command -v docker &> /dev/null; then
    echo "Error: Docker is not installed or not in PATH"
    echo "Please install Docker: https://docs.docker.com/get-docker/"
    exit 1
fi

# Get the directory of this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Clean previous build
rm -rf "$SCRIPT_DIR/bin" "$SCRIPT_DIR/obj"

# Build and package with Docker using ARM64 platform
docker run --rm --platform linux/arm64 \
  -v "$SCRIPT_DIR":/workspace \
  -w /workspace \
  mcr.microsoft.com/dotnet/sdk:8.0-alpine \
  sh -c "apk add --no-cache zip && \
         dotnet tool install -g Amazon.Lambda.Tools || true && \
         export PATH=\"\$PATH:/root/.dotnet/tools\" && \
         dotnet lambda package -o bin/function.zip --function-architecture arm64"

if [ -f "$SCRIPT_DIR/bin/function.zip" ]; then
    echo "✓ Build complete: bin/function.zip"
    ls -lh "$SCRIPT_DIR/bin/function.zip"
else
    echo "✗ Build failed: bin/function.zip not found"
    exit 1
fi
