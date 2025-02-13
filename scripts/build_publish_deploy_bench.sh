#!/bin/bash

set -eu pipefail

if [ -z "$I_KNOW_WHAT_I_AM_DOING" ]; then
    # This script differs from our most recent updates to the gitlab build
    # pipelines. We are going to fix it, but you can help! Either you know what
    # you are doing and can let this script know, or you could update the
    # script yourself, or at least let us know that you want it to be updated!
    echo "Please set I_KNOW_WHAT_I_AM_DOING to 'true' to run this script"
    exit 1
fi

export ARCHITECTURE="${ARCHITECTURE:-amd64}"
export REGION="${REGION:-sa-east-1}"
export AWS_PROFILE="${AWS_PROFILE:-sso-serverless-sandbox-account-admin}"

# use username to avoid conflicts
if [ -z "${USER_NAME}" ]; then
    echo "USER_NAME not set"
    exit 1
fi

export DEPLOYMENT_BUCKET="$USER_NAME-s3-sa-east-1" # need to create a bucket
export SERVICE_NAME="$USER_NAME-test"
export FUNCTION_NAME="$USER_NAME-test"
export SUFFIX=$USER_NAME-testing
export LAYER_NAME="Datadog-Extension-$SUFFIX"

if [ -z "${DD_API_KEY+set}" ]; then
    echo "DD_API_KEY not set"
    exit 1
fi

LATEST_VERSION=$(aws-vault exec $AWS_PROFILE -- aws lambda list-layer-versions --region "$REGION" --layer-name "${LAYER_NAME}" --query 'LayerVersions[0].Version || `0`')
export VERSION=$(($LATEST_VERSION+1))

export EXTENSION_ARN="arn:aws:lambda:$REGION:425362996713:layer:$LAYER_NAME:$VERSION"

# publish_sandbox.sh also builds the layer
echo "y" | ./scripts/publish_sandbox.sh

(cd scripts/deploy && aws-vault exec $AWS_PROFILE -- sls deploy)

(cd scripts && ./bench.sh)
