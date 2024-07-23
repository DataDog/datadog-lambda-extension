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
    printf "Building bottlecap"
    BUILD_FILE=Dockerfile.bottlecap.build
else
    echo "Building bottlecap for alpine"
    BUILD_FILE=Dockerfile.bottlecap.alpine.build
    BUILD_SUFFIX="-alpine"
fi

prepare_folders() {
    # Move into the root directory, so this script can be called from any directory
    SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
    ROOT_DIR=$SCRIPTS_DIR/../..
    cd $ROOT_DIR

    echo $ROOT_DIR

    EXTENSION_DIR=".layers"
    TARGET_DIR=$(pwd)/$EXTENSION_DIR

    rm -rf $EXTENSION_DIR/datadog_bottlecap-${ARCHITECTURE}${BUILD_SUFFIX} 2>/dev/null
    rm -rf $EXTENSION_DIR/datadog_bottlecap-${ARCHITECTURE}${BUILD_SUFFIX}.zip 2>/dev/null

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
        -t datadog/build-bottlecap-${arch} \
        -f ./scripts/${file} \
        --build-arg PLATFORM=$PLATFORM \
        --build-arg GO_AGENT_PATH="datadog_extension-${arch}${BUILD_SUFFIX}" \
        . -o $TARGET_DIR/datadog_bottlecap-${arch}${BUILD_SUFFIX}

    cp $TARGET_DIR/datadog_bottlecap-${arch}${BUILD_SUFFIX}/datadog_extension.zip $TARGET_DIR/datadog_bottlecap-${arch}${BUILD_SUFFIX}.zip

    unzip $TARGET_DIR/datadog_bottlecap-${arch}${BUILD_SUFFIX}/datadog_extension.zip -d $TARGET_DIR/datadog_bottlecap-${arch}${BUILD_SUFFIX}
    rm -rf $TARGET_DIR/datadog_bottlecap-${arch}${BUILD_SUFFIX}/datadog_extension.zip
    rm -rf $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}
    rm -rf $TARGET_DIR/datadog_extension-${arch}${BUILD_SUFFIX}.zip
}

prepare_folders
docker_build $ARCHITECTURE $BUILD_FILE
