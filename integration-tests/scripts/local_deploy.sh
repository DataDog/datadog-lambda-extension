#!/bin/bash

# local_deploy.sh - Deploy a CDK stack to sandbox
# Usage: ./scripts/local_deploy.sh <stack-name>

set -e

if [ -z "$1" ]; then
  echo "Error: Stack name required"
  echo "Usage: ./scripts/local_deploy.sh <stack-name>"
  echo "  stack-name: Name of the CDK stack to deploy (e.g., ExampleTestStack)"
  exit 1
fi

STACK_NAME=$1

IDENTIFIER="local-testing"
USERNAME=$(whoami 2>/dev/null || echo "")
if [ -n "$USERNAME" ]; then
    IDENTIFIER=$(echo "$USERNAME" | cut -d'.' -f1)
fi

echo "Getting latest version of Datadog-Extension-ARM-$IDENTIFIER layer..."
export EXTENSION_LAYER_ARN=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda list-layer-versions --layer-name Datadog-Extension-ARM-$IDENTIFIER --query 'LayerVersions[0].LayerVersionArn' --output text)

if [ -z "$EXTENSION_LAYER_ARN" ]; then
  echo "Error: Failed to retrieve extension layer ARN"
  exit 1
fi

echo "Using extension layer: $EXTENSION_LAYER_ARN"

FULL_STACK_NAME="integ-$IDENTIFIER-$STACK_NAME"
echo "Deploying stack: $FULL_STACK_NAME"

# Get the directory of this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Build Lambda functions based on stack name
echo ""
echo "Building Lambda functions for $STACK_NAME..."
case "$STACK_NAME" in
  *java*)
    "$SCRIPT_DIR/build-java.sh" lambda/base-java
    "$SCRIPT_DIR/build-java.sh" lambda/otlp-java
    ;;
  *dotnet*)
    "$SCRIPT_DIR/build-dotnet.sh" lambda/base-dotnet
    "$SCRIPT_DIR/build-dotnet.sh" lambda/otlp-dotnet
    ;;
  *python*)
    "$SCRIPT_DIR/build-python.sh" lambda/base-python
    "$SCRIPT_DIR/build-python.sh" lambda/otlp-python
    ;;
  *node*)
    "$SCRIPT_DIR/build-node.sh" lambda/base-node
    "$SCRIPT_DIR/build-node.sh" lambda/otlp-node
    ;;
  *)
    echo "Warning: Unknown stack type, skipping Lambda build"
    ;;
esac

echo ""
echo "Building CDK TypeScript and deploying..."
# Build CDK TypeScript and deploy
npm run build && aws-vault exec sso-serverless-sandbox-account-admin -- cdk deploy "$FULL_STACK_NAME" --require-approval never

