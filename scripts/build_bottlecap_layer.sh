#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

# Usage examples :
# ARCHITECTURE=amd64 VERSION=100 RACE_DETECTION_ENABLED=true ./scripts/build_binary_and_layer_dockerized.sh
# or
# VERSION=100 AGENT_VERSION=6.11.0 ./scripts/build_binary_and_layer_dockerized.sh

set -e

if [ -z "$ARCHITECTURE" ]; then
    printf "[ERROR]: ARCHITECTURE not specified\n"
    exit 1
fi

FILE_SUFFIX=$ARCHITECTURE

PLATFORM="aarch64"
if [ "$ARCHITECTURE" == "amd64" ]; then
    PLATFORM="x86_64"
fi

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ROOT_DIR=$SCRIPTS_DIR/..
cd $ROOT_DIR

LAYERS_DIR=".layers"
BUILD_DIR=$ROOT_DIR/$LAYERS_DIR
EXTENSION_PATH=$BUILD_DIR/datadog_extension-$FILE_SUFFIX

mkdir -p $LAYERS_DIR
rm -rf ${EXTENSION_PATH} 2>/dev/null

cd $ROOT_DIR

ALPINE=${ALPINE:-0} \
ARCHITECTURE=$ARCHITECTURE \
FILE_SUFFIX=$FILE_SUFFIX \
PLATFORM=$PLATFORM \
PROFILE=${PROFILE:-release} \
.gitlab/scripts/compile_bottlecap.sh

docker buildx build --platform linux/${ARCHITECTURE} \
    -t datadog/build-bottlecap-${FILE_SUFFIX} \
    -f ./images/Dockerfile.bottlecap.dev \
    --build-arg FILE_SUFFIX=$FILE_SUFFIX \
    . -o $EXTENSION_PATH

cp $EXTENSION_PATH/datadog_extension.zip $EXTENSION_PATH.zip

unzip $EXTENSION_PATH/datadog_extension.zip -d $EXTENSION_PATH
rm -rf $EXTENSION_PATH/datadog_extension.zip
