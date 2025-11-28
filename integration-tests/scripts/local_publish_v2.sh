#!/bin/bash

# local_publish_v2.sh - FAST publish for local testing (optimized Docker build)
# Usage: ./scripts/local_publish_v2.sh
#
# This version optimizes the Docker build process:
# 1. Uses Docker only for compilation (leverages build cache)
# 2. Creates zip locally (faster than Docker buildx)
# 3. Uses debug mode for faster compilation

set -e

# Determine identifier: extract first name from whoami, else fallback to 'local-testing'
IDENTIFIER="local-testing"
USERNAME=$(whoami 2>/dev/null || echo "")
if [ -n "$USERNAME" ]; then
    IDENTIFIER=$(echo "$USERNAME" | cut -d'.' -f1)
fi

# Add -v2 suffix
IDENTIFIER="${IDENTIFIER}-v2"

echo "🚀 Fast publishing extension with identifier: $IDENTIFIER"
echo "Layer name will be: Datadog-Extension-ARM-$IDENTIFIER"

# Save current directory
ORIGINAL_DIR=$(pwd)

# Navigate to the extension root
EXTENSION_ROOT="/Users/john.chrostek/Projects/lambda-agent/datadog-lambda-extension"
cd "$EXTENSION_ROOT"

# Compile using Docker (with build cache for speed)
# Note: Using release mode to get smaller binaries (debug builds are 273MB vs ~30MB release)
echo "📦 Compiling bottlecap with Docker (release mode for smaller size)..."
FIPS=0 ARCHITECTURE=arm64 ALPINE=0 DEBUG=0 FILE_SUFFIX=arm64 PLATFORM=aarch64 .gitlab/scripts/compile_bottlecap.sh

# Prepare layer structure locally (much faster than Docker)
LAYERS_DIR="$EXTENSION_ROOT/.layers"
BUILD_DIR="$LAYERS_DIR/datadog_extension-arm64-v2"
LAYER_ZIP="$LAYERS_DIR/datadog_extension-arm64-v2.zip"

echo "🗂️  Preparing layer structure..."
rm -rf "$BUILD_DIR" "$LAYER_ZIP" 2>/dev/null || true
mkdir -p "$BUILD_DIR/extensions"

# Copy the compiled binary
cp ".binaries/bottlecap-arm64" "$BUILD_DIR/extensions/datadog-agent"
chmod +x "$BUILD_DIR/extensions/datadog-agent"

# Copy the wrapper script
cp "$EXTENSION_ROOT/scripts/datadog_wrapper" "$BUILD_DIR/"
chmod +x "$BUILD_DIR/datadog_wrapper"

# Create the zip file locally (faster than Docker buildx)
echo "📦 Creating layer zip..."
cd "$BUILD_DIR"
zip -r "$LAYER_ZIP" extensions datadog_wrapper > /dev/null

# Publish to Lambda
LAYER_NAME="Datadog-Extension-ARM-$IDENTIFIER"
REGION="us-east-1"

echo "☁️  Publishing layer $LAYER_NAME to $REGION..."
NEW_VERSION=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda publish-layer-version \
    --layer-name "$LAYER_NAME" \
    --description "Datadog Extension for local testing (fast build)" \
    --zip-file "fileb://$LAYER_ZIP" \
    --region "$REGION" | jq -r '.Version')


echo ""
echo "=========================================="
echo "✅ DONE: Published version $NEW_VERSION"
echo "=========================================="

# Return to original directory
cd "$ORIGINAL_DIR"
