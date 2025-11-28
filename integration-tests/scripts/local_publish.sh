#!/bin/bash

# local_publish.sh - Publish the Datadog extension for local testing
# Usage: ./scripts/local_publish.sh

set -e

# Determine identifier: extract first name from whoami, else fallback to 'local-testing'
IDENTIFIER="local-testing"
USERNAME=$(whoami 2>/dev/null || echo "")
if [ -n "$USERNAME" ]; then
    IDENTIFIER=$(echo "$USERNAME" | cut -d'.' -f1)
fi

echo "Publishing extension with identifier: $IDENTIFIER"
echo "Layer name will be: Datadog-Extension-ARM-$IDENTIFIER"

# Save current directory
ORIGINAL_DIR=$(pwd)

# Navigate to the extension root directory
cd /Users/john.chrostek/Projects/lambda-agent/datadog-lambda-extension

# Build and publish for ARM64 with custom layer name
LAYER_PATH=".layers/datadog_extension-arm64.zip"
LAYER_NAME="Datadog-Extension-ARM-$IDENTIFIER"
REGION="us-east-1"

echo "Building extension for arm64 (debug mode for faster builds)..."
FIPS=0 DEBUG=1 ARCHITECTURE=arm64 ./scripts/build_bottlecap_layer.sh

echo "Publishing layer $LAYER_NAME to $REGION..."
NEW_VERSION=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda publish-layer-version \
    --layer-name "$LAYER_NAME" \
    --description "Datadog Extension for local testing" \
    --zip-file "fileb://$LAYER_PATH" \
    --region "$REGION" | jq -r '.Version')

echo ""
echo "=========================================="
echo "DONE: Published version $NEW_VERSION"
echo "Layer: $LAYER_NAME"
echo "Region: $REGION"
echo "=========================================="

# Return to original directory
cd "$ORIGINAL_DIR"
