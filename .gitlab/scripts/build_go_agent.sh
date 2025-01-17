#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

# Usage
# ARCHITECTURE=arm64 ./scripts/build_go_agent.sh

set -e

if [ -z "$ARCHITECTURE" ]; then
    printf "[ERROR]: ARCHITECTURE not specified\n"
    exit 1
fi

if [ -z "$AGENT_VERSION" ]; then
    printf "[ERROR]: AGENT_VERSION not specified\n"
    exit 1
else
    printf "Building agent with version: ${AGENT_VERSION}\n"

fi

if [ -z "$CI_COMMIT_TAG" ]; then
    # Running on dev
    printf "Running on dev environment\n"
    VERSION="dev"
else
    printf "Found version tag in environment\n"
    VERSION="${CI_COMMIT_TAG//[!0-9]/}"
    printf "Version: ${VERSION}\n"
fi


if [ -z "$SERVERLESS_INIT" ]; then
    printf "Building Datadog Lambda Extension\n"
    CMD_PATH="cmd/serverless"
else
    printf "Building Serverless Init\n"
    CMD_PATH="cmd/serverless-init"
fi


if [ -z "$ALPINE" ]; then
    BUILD_FILE=Dockerfile.build
else
    printf "Building for alpine\n"
    BUILD_FILE=Dockerfile.alpine.build
    BUILD_SUFFIX="-alpine"
fi

# Allow override build tags
if [ -z "$BUILD_TAGS" ]; then
    BUILD_TAGS="serverless otlp"
fi

# Allow override agent path
if [ -z "$AGENT_PATH" ]; then
    AGENT_PATH="../datadog-agent"
fi

MAIN_DIR=$(pwd) # datadog-lambda-extension

EXTENSION_DIR=".layers"
TARGET_DIR=$MAIN_DIR/$EXTENSION_DIR

# Make sure the folder does not exist
rm -rf $EXTENSION_DIR 2>/dev/null

mkdir -p $EXTENSION_DIR

# Prepare folder with only *mod and *sum files to enable Docker caching capabilities
mkdir -p $MAIN_DIR/scripts/.src $MAIN_DIR/scripts/.cache
printf "Copy mod files to build a cache\n"
cp $AGENT_PATH/go.mod $MAIN_DIR/scripts/.cache
cp $AGENT_PATH/go.sum $MAIN_DIR/scripts/.cache

# Compress all files to speed up docker copy
touch $MAIN_DIR/scripts/.src/datadog-agent.tgz
cd $AGENT_PATH/..
tar --exclude=.git -czf $MAIN_DIR/scripts/.src/datadog-agent.tgz datadog-agent
cd $MAIN_DIR

function docker_build {
    arch=$1
    file=$2

    docker buildx build --platform linux/${arch} \
        -t datadog/build-go-agent-${arch}:${VERSION} \
        -f ${MAIN_DIR}/scripts/${file} \
        --build-arg EXTENSION_VERSION="${VERSION}" \
        --build-arg AGENT_VERSION="${AGENT_VERSION}" \
        --build-arg CMD_PATH="${CMD_PATH}" \
        --build-arg BUILD_TAGS="${BUILD_TAGS}" \
        . -o $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}
    
    cp $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}/datadog_extension.zip $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}.zip
    unzip $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}/datadog_extension.zip -d $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}
    rm -rf $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}/datadog_extension.zip
}

docker_build $ARCHITECTURE $BUILD_FILE

