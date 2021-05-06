#!/bin/sh

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2020 Datadog, Inc.

# Builds Datadogpy layers for lambda functions, using Docker
set -e

# Move into the root directory, so this script can be called from any directory
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $DIR/..

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
cd ~/dd/datadog-agent/cmd/serverless
GOOS=linux go build -ldflags="-s -w" -tags serverless -o $TARGET_DIR/datadog-agent
if [ "$COMPRESS" = true ]; then
    upx --brute $TARGET_DIR/datadog-agent
fi

cd -
rm -rf "./var"

echo "Building Lambda layer"
zip -q -r "${LAYER_DIR}/${LAYER_FILE}" -r $EXTENSION_DIR

echo "Done!"
