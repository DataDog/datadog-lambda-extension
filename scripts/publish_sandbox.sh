#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

# Usage: VERSION=5 ARCHITECTURE=[amd64|arm64] ./scripts/publish_sandbox.sh

# Optional environment variables:
# VERSION - Use a specific version number. By default, increment the version by 1.
# ARCHITECTURE - Which architecture to build for (amd64 or arm64). The default is amd64.
# RELEASE_CANDIDATE - If true, publish as a release candidate to us-east-1. The default is false.

set -e

if [ -z $ARCHITECTURE ]; then
    echo "No architecture specified, defaulting to amd64"
    ARCHITECTURE="amd64"
fi

if [ "$ARCHITECTURE" != "amd64" -a "$ARCHITECTURE" != "arm64" ]; then
    echo "ERROR: Invalid architecture (must be amd64 OR arm64 for sandbox deploys)"
    exit 1
fi

if [ "$ARCHITECTURE" == "amd64" ]; then
    LAYER_NAME="Datadog-Extension"
fi
if [ "$ARCHITECTURE" == "arm64" ]; then
    LAYER_NAME="Datadog-Extension-ARM"
fi

if [ ! -z "$SUFFIX" ]; then
   LAYER_NAME+="-$SUFFIX"
fi

REGION="sa-east-1"
if [ "$RELEASE_CANDIDATE" == "true" ]; then
    echo "Will publish as a release candidate to us-east-1"
    REGION="us-east-1"
fi

if [ -z $VERSION ]; then
    echo "No version specified, automatically incrementing version number"

    LAST_LAYER_VERSION=$(
        aws-vault exec sso-serverless-sandbox-account-admin -- \
        aws lambda list-layer-versions \
            --layer-name $LAYER_NAME \
            --region $REGION \
        | jq -r ".LayerVersions | .[0] |  .Version" \
    )
    if [ "$LAST_LAYER_VERSION" == "null" ]; then
        echo "Error auto-detecting the last layer version"
        exit 1
    else
        VERSION=$(($LAST_LAYER_VERSION+1))
        echo "Will publish new layer version as $VERSION"
    fi
fi

# Move into the root directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

VERSION=$VERSION ARCHITECTURE=$ARCHITECTURE ./scripts/build_binary_and_layer_dockerized.sh
VERSION=$VERSION ARCHITECTURE=$ARCHITECTURE REGIONS=$REGION aws-vault exec sso-serverless-sandbox-account-admin -- ./scripts/publish_layers.sh
