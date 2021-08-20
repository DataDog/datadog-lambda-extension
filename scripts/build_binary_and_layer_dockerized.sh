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
    AGENT_PATH="../datadog-agent"
else
    AGENT_PATH="/home/runner/work/datadog-lambda-extension/datadog-lambda-extension/datadog-agent"
fi

BASE_PATH=$(pwd)
EXTENSION_DIR=".layers"
TARGET_DIR=$(pwd)/$EXTENSION_DIR

mkdir -p $EXTENSION_DIR

echo $BASE_PATH

# First prepare a folder with only *mod and *sum files to enable Docker caching capabilities
mkdir -p $BASE_PATH/scripts/.src $BASE_PATH/scripts/.cache
echo "Copy mod files to build a cache"
cp $AGENT_PATH/go.mod $BASE_PATH/scripts/.cache
cp $AGENT_PATH/go.sum $BASE_PATH/scripts/.cache

echo "Compressing all files to speed up docker copy"
touch $BASE_PATH/scripts/.src/datadog-agent.tgz
cd $AGENT_PATH/..
tar --exclude=.git -czf $BASE_PATH/scripts/.src/datadog-agent.tgz datadog-agent
cd $BASE_PATH
ls -la $BASE_PATH/scripts/.src
ls -la $BASE_PATH/scripts/.cache

DOCKER_BUILDKIT=1 docker build -t datadog/build-lambda-extension:$VERSION \
    -f ./scripts/Dockerfile.build \
    --build-arg EXTENSION_VERSION="${VERSION}" .

dockerId=$(docker create datadog/build-lambda-extension:$VERSION)
docker cp $dockerId:/datadog_extension.zip $TARGET_DIR
