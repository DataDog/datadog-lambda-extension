#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

# Compares layer size to threshold, and fails if below that threshold

set -e

if [ -z "$MAX_LAYER_COMPRESSED_SIZE_MB" ]; then
    printf "[ERROR]: MAX_LAYER_COMPRESSED_SIZE_MB not specified\n"
    exit 1
fi

if [ -z "$MAX_LAYER_UNCOMPRESSED_SIZE_MB" ]; then
    printf "[ERROR]: MAX_LAYER_UNCOMPRESSED_SIZE_MB not specified\n"
    exit 1
fi

validate_size() {
  local max_size=$1
  local file_size=$2
    if [ "$file_size" -gt "$max_size" ]; then
        echo "Size $file_size exceeded limit $max_size kb"
        exit 1
    fi
}

if [ -z "$LAYER_FILE" ]; then
    echo "[ERROR]: LAYER_FILE not specified"
    exit 1
fi

MAX_LAYER_COMPRESSED_SIZE_KB=$(( $MAX_LAYER_COMPRESSED_SIZE_MB * 1024))
MAX_LAYER_UNCOMPRESSED_SIZE_KB=$(( $MAX_LAYER_UNCOMPRESSED_SIZE_MB * 1024 ))

FILE=".layers"/$LAYER_FILE
FILE_SIZE=$(stat --printf="%s" "$FILE")
FILE_SIZE_KB="$(( ${FILE_SIZE%% *} / 1024))"

mkdir tmp
unzip -q "$FILE" -d tmp
UNZIPPED_FILE_SIZE=$(du -shb tmp/ | cut -f1)
UNZIPPED_FILE_SIZE_KB="$(( ${UNZIPPED_FILE_SIZE%% *} / 1024))"
rm -rf tmp

echo "Layer file ${FILE} has zipped size ${FILE_SIZE_KB} kb"
echo "Layer file ${FILE} has unzipped size ${UNZIPPED_FILE_SIZE_KB} kb"

validate_size "$MAX_LAYER_COMPRESSED_SIZE_KB" $FILE_SIZE_KB
validate_size "$MAX_LAYER_UNCOMPRESSED_SIZE_KB" $UNZIPPED_FILE_SIZE_KB
