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

if [ -z "$ALPINE" ]; then
    printf "[ERROR]: ALPINE not specified\n"
    exit 1
else
    printf "Alpine compile requested: ${ALPINE}\n"
fi

if [ "$ALPINE" = "0" ]; then
    COMPILE_IMAGE=Dockerfile.bottlecap.compile
else
    printf "Compiling for alpine\n"
    COMPILE_IMAGE=Dockerfile.bottlecap.alpine.compile
    FILE_SUFFIX=${FILE_SUFFIX}-alpine
fi


# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ROOT_DIR=$SCRIPTS_DIR/../..
cd $ROOT_DIR

BINARY_DIR=".binaries"
TARGET_DIR=$(pwd)/$BINARY_DIR
BINARY_PATH=$TARGET_DIR/compiled-bottlecap-$FILE_SUFFIX

mkdir -p $BINARY_DIR

cd $ROOT_DIR

docker_build() {
    local arch=$1
    local file=$2
    if [ "$arch" == "amd64" ]; then
        PLATFORM="x86_64"
    else
        PLATFORM="aarch64"
    fi

    docker buildx build --platform linux/${arch} \
        -t datadog/compile-bottlecap \
        -f ./images/${file} \
        --build-arg PLATFORM=$PLATFORM \
        . -o $BINARY_PATH

    # Copy the compiled binary to the target directory with the expected name
    # If it already exist, it will be replaced
    cp $BINARY_PATH/bottlecap $TARGET_DIR/bottlecap-$FILE_SUFFIX
}

docker_build $ARCHITECTURE $COMPILE_IMAGE
