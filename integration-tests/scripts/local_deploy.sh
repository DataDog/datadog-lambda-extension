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

FULL_STACK_NAME="IntegrationTests-$IDENTIFIER-$STACK_NAME"
echo "Deploying stack: $FULL_STACK_NAME"

# Build and deploy
npm run build && aws-vault exec sso-serverless-sandbox-account-admin -- cdk deploy "$FULL_STACK_NAME" --require-approval never

