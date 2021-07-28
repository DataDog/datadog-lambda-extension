#!/bin/sh

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

set -e


# Ensure the target extension version is defined
if [ -z "$VERSION" ]; then
    echo "Extension version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
fi

if [ -z "$CI" ]; then
    SERVERLESS_CMD_PATH="../datadog-agent/cmd/serverless"
else
    SERVERLESS_CMD_PATH="/home/runner/work/datadog-lambda-extension/datadog-lambda-extension/datadog-agent/cmd/serverless"
fi

# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

LAYER_DIR=".layers"
LAYER_FILE="datadog_extension"
EXTENSION_DIR="extensions"
TMP_DIR='./var/task'

rm -rf $LAYER_DIR
rm -rf $EXTENSION_DIR
rm -rf $TMP_DIR
mkdir $LAYER_DIR
mkdir $EXTENSION_DIR
mkdir -p $TMP_DIR

TARGET_DIR=$(pwd)/$EXTENSION_DIR

echo "Building Lambda extension binary"
cd $SERVERLESS_CMD_PATH

# Add the current version number to the tags package before compilation
LD_FLAGS="-X github.com/DataDog/datadog-agent/pkg/serverless/tags.currentExtensionVersion=${VERSION}"

if [ "$PRODUCTION" = true ]; then
    LD_FLAGS+=" -s -w"
fi

GOOS=linux go build -ldflags="${LD_FLAGS}" -tags serverless -o $TARGET_DIR/datadog-agent

cd $SCRIPTS_DIR/..
rm -rf "./var"

echo "Building Lambda layer"
zip -q -r "${LAYER_DIR}/${LAYER_FILE}" -r $EXTENSION_DIR

echo "Done!"
