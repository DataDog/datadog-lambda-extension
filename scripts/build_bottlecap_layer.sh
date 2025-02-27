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

prepare_folders() {
  # Move into the root directory, so this script can be called from any directory
  SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
  ROOT_DIR=$SCRIPTS_DIR/..
  cd $ROOT_DIR

  EXTENSION_DIR=".layers"
  TARGET_DIR=$(pwd)/$EXTENSION_DIR

  # Make sure the folder does not exist
  rm -rf $EXTENSION_DIR 2>/dev/null

  mkdir -p $EXTENSION_DIR
  cd $ROOT_DIR
}


_docker_build_bottlecap_zip() {
    local arch=$1
    if [ "$arch" == "amd64" ]; then
        PLATFORM="x86_64"
    else
        PLATFORM="aarch64"
    fi

    docker buildx build --platform linux/${arch} \
        -t datadog/build-bottlecap-${arch} \
        -f ./scripts/Dockerfile.bottlecap.dev \
        --build-arg PLATFORM=$PLATFORM \
        . -o $TARGET_DIR/datadog_bottlecap-${arch}

    cp $TARGET_DIR/datadog_bottlecap-${arch}/datadog_extension.zip $TARGET_DIR/datadog_bottlecap-${arch}.zip

    unzip $TARGET_DIR/datadog_bottlecap-${arch}/datadog_extension.zip -d $TARGET_DIR/datadog_bottlecap-${arch}
    rm -rf $TARGET_DIR/datadog_bottlecap-${arch}/datadog_extension.zip
}

build_for_arch() {
  if [ "$ARCHITECTURE" == "amd64" ]; then
      echo "Building for amd64 only"
      _docker_build_bottlecap_zip amd64
  elif [ "$ARCHITECTURE" == "arm64" ]; then
      echo "Building for arm64 only"
      _docker_build_bottlecap_zip arm64
  else
      echo "Building for both amd64 and arm64"
      _docker_build_bottlecap_zip amd64
      _docker_build_bottlecap_zip arm64
  fi
}

prepare_folders
build_for_arch
