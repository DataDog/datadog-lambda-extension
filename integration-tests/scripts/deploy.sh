#!/bin/bash

# deploy.sh - Deploy a CDK stack to sandbox
# Usage: ./scripts/deploy.sh <stack-name> [stack-suffix]

set -e

if [ -z "$1" ]; then
  echo "Error: Stack name required"
  echo "Usage: ./scripts/deploy.sh <stack-name> [stack-suffix]"
  echo "  stack-name: Name of the CDK stack to deploy (e.g., ExampleTestStack)"
  echo "  stack-suffix: Suffix for the stack (e.g., 'john'). If not provided, uses first name from whoami"
  exit 1
fi

STACK_NAME=$1

# Determine suffix: use argument, else extract first name from whoami, else fallback to 'local-testing'
if [ -n "$2" ]; then
    SUFFIX="$2"
else
    USERNAME=$(whoami 2>/dev/null || echo "")
    if [ -n "$USERNAME" ]; then
        # Extract first name (part before the dot)
        SUFFIX=$(echo "$USERNAME" | cut -d'.' -f1)
    else
        SUFFIX="local-testing"
    fi
fi

echo "Deploying stack: $STACK_NAME-$SUFFIX"

echo "Getting latest version of Datadog-Extension-ARM-$SUFFIX layer..."
export EXTENSION_LAYER_ARN=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda list-layer-versions --layer-name Datadog-Extension-ARM-$SUFFIX --query 'LayerVersions[0].LayerVersionArn' --output text)

if [ -z "$EXTENSION_LAYER_ARN" ]; then
  echo "Error: Failed to retrieve extension layer ARN"
  exit 1
fi

echo "Using extension layer: $EXTENSION_LAYER_ARN"

# Set suffix environment variable for CDK app
export SUFFIX=$SUFFIX

# Build and deploy
npm run build && aws-vault exec sso-serverless-sandbox-account-admin -- cdk deploy "$STACK_NAME-$SUFFIX" --require-approval never

