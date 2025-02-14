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

if [ -z "$ALPINE" ]; then
    printf "[ERROR]: ALPINE not specified\n"
    exit 1
else
    printf "Alpine compile requested: ${ALPINE}\n"
fi


if [ "$ALPINE" = "0" ]; then
    COMPILE_FILE=Dockerfile.bottlecap.compile
else
    printf "Compiling for alpine\n"
    COMPILE_FILE=Dockerfile.bottlecap.alpine.compile
fi

prepare_folders() {
    # Move into the root directory, so this script can be called from any directory
    SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
    ROOT_DIR=$SCRIPTS_DIR/../..
    cd $ROOT_DIR

    echo $ROOT_DIR

    BINARY_DIR=".binaries"
    TARGET_DIR=$(pwd)/$BINARY_DIR

    rm -rf $BINARY_DIR 2>/dev/null
    mkdir -p $BINARY_DIR

    cd $ROOT_DIR
}


docker_build() {
    local arch=$1
    local file=$2
    if [ "$arch" == "amd64" ]; then
        PLATFORM="x86_64"
    else
        PLATFORM="aarch64"
    fi

    docker buildx build --platform linux/${arch} \
        -t datadog/compile-bottlecap-${SUFFIX} \
        -f .gitlab/scripts/${file} \
        --build-arg PLATFORM=$PLATFORM \
        . -o $TARGET_DIR/compiled-bottlecap-${SUFFIX}

    cp $TARGET_DIR/compiled-bottlecap-${SUFFIX}/bottlecap $TARGET_DIR/bottlecap-${SUFFIX}
}

prepare_folders
docker_build $ARCHITECTURE $COMPILE_FILE

