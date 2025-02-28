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

FILE_SUFFIX=$ARCHITECTURE

# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ROOT_DIR=$SCRIPTS_DIR/../..
cd $ROOT_DIR

EXTENSION_DIR=".layers"
TARGET_DIR=$(pwd)/$EXTENSION_DIR
EXTENSION_PATH=$TARGET_DIR/datadog_extension-${FILE_SUFFIX}

mkdir -p $EXTENSION_DIR
rm -rf ${EXTENSION_PATH} 2>/dev/null

cd $ROOT_DIR

docker_build() {
    local arch=$1

    docker buildx build --platform linux/${arch} \
        -t datadog/build-extension-${FILE_SUFFIX} \
        -f ./images/Dockerfile.build_layer \
        --build-arg FILE_SUFFIX=$FILE_SUFFIX \
        . -o $EXTENSION_PATH

    cp $EXTENSION_PATH/datadog_extension.zip $TARGET_DIR/datadog_extension-${FILE_SUFFIX}.zip
    unzip $EXTENSION_PATH/datadog_extension.zip -d $TARGET_DIR/datadog_extension-${FILE_SUFFIX}
    rm -rf ${EXTENSION_PATH}
}

docker_build $ARCHITECTURE
