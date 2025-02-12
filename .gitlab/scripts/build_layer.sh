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

prepare_folders() {
    # Move into the root directory, so this script can be called from any directory
    SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
    ROOT_DIR=$SCRIPTS_DIR/../..
    cd $ROOT_DIR

    echo $ROOT_DIR

    EXTENSION_DIR=".layers"
    TARGET_DIR=$(pwd)/$EXTENSION_DIR

    rm -rf ${EXTENSION_DIR} 2>/dev/null
    mkdir -p $EXTENSION_DIR

    cd $ROOT_DIR
}


docker_build() {
    local arch=$1

    docker buildx build --platform linux/${arch} \
        -t datadog/build-extension-${SUFFIX} \
        -f ./scripts/Dockerfile.build_layer \
        --build-arg SUFFIX=$SUFFIX \
        . -o $TARGET_DIR/datadog-extension-${SUFFIX}

    cp $TARGET_DIR/datadog-extension-${SUFFIX}/datadog_extension.zip $TARGET_DIR/datadog_extension-${SUFFIX}.zip
    unzip $TARGET_DIR/datadog-extension-${SUFFIX}/datadog_extension.zip -d $TARGET_DIR/datadog_extension-${SUFFIX}
    rm -rf $TARGET_DIR/datadog-extension-${SUFFIX}/
}

prepare_folders
docker_build $ARCHITECTURE
