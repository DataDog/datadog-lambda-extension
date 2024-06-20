#!/bin/bash

set -eu pipefail

export VERSION="${VERSION:-0}"
export PUBLISH="${PUBLISH:-true}"
export DEPLOY="${DEPLOY:-true}"
export ARCHITECTURE="${ARCHITECTURE:-amd64}"
export REGION="${REGION:-sa-east-1}"
export AWS_PROFILE="${AWS_PROFILE:-sso-serverless-sandbox-account-admin}"
export PRINT_BINARY_SIZE="${PRINT_BINARY_SIZE:-true}"

# use username to avoid conflicts
if [ -z "${USER_NAME}" ]; then
    echo "USER_NAME not set"
    exit 1
fi

export DEPLOYMENT_BUCKET="$USER_NAME-sa-east-1" # need to create a bucket
export SERVICE_NAME="${FUNCTION_NAME:-$USER_NAME-test}"
export FUNCTION_NAME="${FUNCTION_NAME:-$USER_NAME-test}"
export SUFFIX=$FUNCTION_NAME
export LAYER_NAME="Datadog-Extension-$SUFFIX"

if [ -z "${DD_API_KEY+set}" ]; then
    echo "DD_API_KEY not set"
    exit 1
fi

if [ "${VERSION}" == 0 ]; then
    echo "Fetching latest version of layer $LAYER_NAME"
    LATEST_VERSION=$(aws-vault exec $AWS_PROFILE -- aws lambda list-layer-versions --region "$REGION" --layer-name "${LAYER_NAME}" --query 'LayerVersions[0].Version || `0`')
    export VERSION=$(($LATEST_VERSION+1))
    echo "New version $VERSION"
fi

export EXTENSION_ARN="arn:aws:lambda:$REGION:425362996713:layer:$LAYER_NAME:$VERSION"

if [ "${PUBLISH}" == true ]; then
    # publish_sandbox.sh also builds the layer
    echo "Publishing layer $LAYER_NAME version $VERSION"
    echo "y" | ./scripts/publish_sandbox.sh
fi

if [ "${DEPLOY}" == true ]; then
    echo "Deploying layer $LAYER_NAME version $VERSION"
    (cd scripts/deploy && aws-vault exec $AWS_PROFILE -- sls deploy)
fi

(cd scripts && ./bench.sh)
