#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

set -e

if [ -z "$ARCHITECTURE" ]; then
    printf "[ERROR]: ARCHITECTURE not specified\n"
    exit 1
fi

if [ -z "$FILE_SUFFIX" ]; then
    printf "[WARNING] No FILE_SUFFIX provided, using ${ARCHITECTURE}\n"
    FILE_SUFFIX=$ARCHITECTURE
fi

if [ -z "$AGENT_VERSION" ]; then
    printf "[ERROR]: AGENT_VERSION not specified\n"
    exit 1
else
    printf "Compiling agent with version: ${AGENT_VERSION}\n"
fi

if [ -z "$ALPINE" ]; then
    printf "[ERROR]: ALPINE not specified\n"
    exit 1
else
    printf "Alpine compile requested: ${ALPINE}\n"
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


if [ "$ALPINE" = "0" ]; then
    COMPILE_IMAGE=Dockerfile.go_agent.compile
else
    printf "Compiling for alpine\n"
    COMPILE_IMAGE=Dockerfile.go_agent.alpine.compile
    FILE_SUFFIX=${FILE_SUFFIX}-alpine
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

BINARY_DIR=".binaries"
TARGET_DIR=$MAIN_DIR/$BINARY_DIR
BINARY_PATH=$TARGET_DIR/compiled-datadog-agent-$FILE_SUFFIX

mkdir -p $BINARY_DIR

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

function docker_compile {
    arch=$1
    file=$2

    docker buildx build --platform linux/${arch} \
        -t datadog/compile-go-agent:${VERSION} \
        -f ${MAIN_DIR}/images/${file} \
        --build-arg EXTENSION_VERSION="${VERSION}" \
        --build-arg AGENT_VERSION="${AGENT_VERSION}" \
        --build-arg BUILD_TAGS="${BUILD_TAGS}" \
        . -o $BINARY_PATH

    # Copy the compiled binary to the target directory with the expected name
    # If it already exist, it will be replaced
    cp $BINARY_PATH/datadog-agent $TARGET_DIR/datadog-agent-$FILE_SUFFIX
}

docker_compile $ARCHITECTURE $COMPILE_IMAGE
