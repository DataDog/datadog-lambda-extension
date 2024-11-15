#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

# Compares layer size to threshold, and fails if below that threshold

set -e

if [ -z "$LAYER_FILE" ]; then
    echo "[ERROR]: LAYER_FILE not specified"
    exit 1
fi

MAX_LAYER_COMPRESSED_SIZE_KB=$(expr 20 \* 1024) # 20 MB, amd64 is 19, while arm64 is 18
MAX_LAYER_UNCOMPRESSED_SIZE_KB=$(expr 50 \* 1024) # 50 MB, amd is 50, while arm64 is 47

LAYERS_DIR=".layers"

FILE=$LAYERS_DIR/$LAYER_FILE
FILE_SIZE=$(stat --printf="%s" $FILE)
FILE_SIZE_KB="$(( ${FILE_SIZE%% *} / 1024))"
echo "Layer file ${FILE} has zipped size ${FILE_SIZE_KB} kb"
if [ "$FILE_SIZE_KB" -gt "$MAX_LAYER_COMPRESSED_SIZE_KB" ]; then
    echo "Zipped size exceeded limit $MAX_LAYER_COMPRESSED_SIZE_KB kb"
    exit 1
fi
mkdir tmp
unzip -q $FILE -d tmp
UNZIPPED_FILE_SIZE=$(du -shb tmp/ | cut -f1)
UNZIPPED_FILE_SIZE_KB="$(( ${UNZIPPED_FILE_SIZE%% *} / 1024))"
rm -rf tmp
echo "Layer file ${FILE} has unzipped size ${UNZIPPED_FILE_SIZE_KB} kb"
if [ "$UNZIPPED_FILE_SIZE_KB" -gt "$MAX_LAYER_UNCOMPRESSED_SIZE_KB" ]; then
    echo "Unzipped size exceeded limit $MAX_LAYER_UNCOMPRESSED_SIZE_KB kb"
    exit 1
fi
