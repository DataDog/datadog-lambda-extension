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

validate_argument() {
  if [ -z "$VERSION" ]; then
      echo "Extension version not specified"
      echo ""
      echo "EXITING SCRIPT."
      exit 1
  fi

  if [ -z "$BUILD_TAGS" ]; then
      BUILD_TAGS="serverless otlp"
  fi

  if [ -z "$AGENT_PATH" ]; then
      AGENT_PATH="../datadog-agent"
  fi
}

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


docker_build_bottlecap_zip() {
    local arch=$1
    if [ "$arch" == "amd64" ]; then
        PLATFORM="x86_64"
    else
        PLATFORM="aarch64"
    fi

    docker build --platform linux/${arch} \
        -t datadog/build-bottlecap-${arch}:$VERSION \
        -f ./scripts/Dockerfile.bottlecap.build \
        --build-arg PLATFORM=$PLATFORM \
        . --load
    local dockerId=$(docker create datadog/build-bottlecap-${arch}:$VERSION)
    docker cp $dockerId:/datadog_extension.zip $TARGET_DIR/datadog_bottlecap-${arch}.zip
    docker rm $dockerId
    unzip $TARGET_DIR/datadog_bottlecap-${arch}.zip -d $TARGET_DIR/datadog_bottlecap-${arch}
}

build_for_arch() {
  if [ "$ARCHITECTURE" == "amd64" ]; then
      echo "Building for amd64 only"
      docker_build_bottlecap_zip amd64
  elif [ "$ARCHITECTURE" == "arm64" ]; then
      echo "Building for arm64 only"
      docker_build_bottlecap_zip arm64
  else
      echo "Building for both amd64 and arm64"
      docker_build_bottlecap_zip amd64
      docker_build_bottlecap_zip arm64
  fi
}

validate_argument
prepare_folders
build_for_arch
