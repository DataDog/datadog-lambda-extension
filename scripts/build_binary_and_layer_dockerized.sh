#!/bin/sh

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

if [ -z "$VERSION" ]; then
    echo "Extension version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
fi

if [ -z "$CI" ]; then
    SERVERLESS_CMD_PATH="../datadog-agent"
else
    SERVERLESS_CMD_PATH="/home/runner/work/datadog-lambda-extension/datadog-lambda-extension/datadog-agent"
fi

EXTENSION_DIR=".layers"
TARGET_DIR=$(pwd)/$EXTENSION_DIR

mkdir -p $EXTENSION_DIR

# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

# First prepare a folder with only *mod and *sum files to enable Docker caching capabilities
mkdir -p ./scripts/.src ./scripts/.cache
echo "Copy mod files to build a cache"
cp $SERVERLESS_CMD_PATH/go.mod ./scripts/.cache
cp $SERVERLESS_CMD_PATH/go.sum ./scripts/.cache

echo "Compressing all files to speed up docker copy"
tar --exclude=$SERVERLESS_CMD_PATH/.git -czfv ./scripts/.src/datadog-agent.tgz $SERVERLESS_CMD_PATH
tar -ztvf ./scripts/.src/datadog-agent.tgz

DOCKER_BUILDKIT=1 docker build -t datadog/build-lambda-extension:$VERSION \
    -f $TARGET_DIR/../scripts/Dockerfile.build \
    --build-arg VERSION=$VERSION .

dockerId=$(docker create datadog/build-lambda-extension:$VERSION)
docker cp $dockerId:/datadog_extension.zip $TARGET_DIR
echo $TARGET_DIR