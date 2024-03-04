#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

# Usage examples :
# ARCHITECTURE=amd64 VERSION=100 RACE_DETECTION_ENABLED=true ./scripts/build_binary_and_layer_dockerized.sh
# or
# VERSION=100 AGENT_VERSION=6.11.0 ./scripts/build_binary_and_layer_dockerized.sh

set -ex

function prepare_tags() {
  if [ -z "$VERSION" ]; then
    echo "Extension version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
  fi

  if [ -z "$SERVERLESS_INIT" ]; then
    CMD_PATH="cmd/serverless"
  else
    CMD_PATH="cmd/serverless-init"
  fi

  if [ -z "$BUILD_TAGS" ]; then
    BUILD_TAGS="serverless otlp"
  fi

  if [ -z "$AGENT_PATH" ]; then
    AGENT_PATH="../datadog-agent"
  fi
}

function prepare_dir() {
  # Move into the root directory, so this script can be called from any directory
  local SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
  local ROOT_DIR=$SCRIPTS_DIR/..
  cd "$ROOT_DIR"

  local EXTENSION_DIR=".layers"
  TARGET_DIR=$(pwd)/$EXTENSION_DIR

  # Make sure the folder does not exist
  rm -rf $EXTENSION_DIR 2>/dev/null

  mkdir -p $EXTENSION_DIR

  # First prepare a folder with only *mod and *sum files to enable Docker caching capabilities
  local CACHE_DIR=$ROOT_DIR/scripts/.cache
  mkdir -p "$ROOT_DIR"/scripts/.src "$CACHE_DIR"
  echo "Copy mod files to build a cache"
  cp $AGENT_PATH/go.mod "$CACHE_DIR"
  cp $AGENT_PATH/go.sum "$CACHE_DIR"

  echo "Compressing all files to speed up docker copy"
  touch "$ROOT_DIR"/scripts/.src/datadog-agent.tgz
  cd $AGENT_PATH/..
  tar --exclude=.git -czf "$ROOT_DIR"/scripts/.src/datadog-agent.tgz datadog-agent
  cd "$ROOT_DIR"
}

function docker_build_zip {
  local arch=$1
  local suffix=$2


  build_rust_shell "$arch"

  DOCKER_BUILDKIT=1 docker buildx build --platform linux/"${arch}" \
    -t datadog/build-lambda-extension-"${arch}":"$VERSION" \
    -f ./scripts/"$BUILD_FILE" \
    --build-arg EXTENSION_VERSION="${VERSION}" \
    --build-arg AGENT_VERSION="${AGENT_VERSION}" \
    --build-arg CMD_PATH="${CMD_PATH}" \
    --build-arg BUILD_TAGS="${BUILD_TAGS}" \
    . --load
  local dockerId=$(docker create datadog/build-lambda-extension-"${arch}":"$VERSION")

  local FULL_FILE_NAME=$TARGET_DIR/datadog_extension-${arch}${suffix}

  docker cp "$dockerId":/datadog_extension.zip "$FULL_FILE_NAME".zip
  unzip "$FULL_FILE_NAME".zip -d "$FULL_FILE_NAME"
}

function build_rust_shell {
  cd ../serverless-agent-shell
  ./build.sh "$1"
  cd -
  cp ../serverless-agent-shell/lambda-extension-shell-"$1" scripts/.src/lambda-extension-shell
}

prepare_tags
prepare_dir

BUILD_FILE=Dockerfile.build
if [ "$RACE_DETECTION_ENABLED" = "true" ]; then
  BUILD_FILE=Dockerfile.race.build
fi

if [[ "$SERVERLESS_INIT" == "true" && "$ALPINE" == "false" ]]; then
  echo "Building serverless init for non-alpine amd64 & arm64"
  docker_build_zip amd64
  docker_build_zip arm64
elif [[ "$SERVERLESS_INIT" == "true" && "$ALPINE" == "true" ]]; then
  echo "Building serverless init for alpine amd64 & arm64"
  BUILD_FILE=Dockerfile.alpine.build
  docker_build_zip amd64 -alpine
  docker_build_zip arm64 -alpine
elif [ "$ARCHITECTURE" == "amd64" ]; then
  echo "Building for amd64 only"
  docker_build_zip amd64
  BUILD_FILE=Dockerfile.alpine.build
  docker_build_zip amd64 -alpine
elif [ "$ARCHITECTURE" == "arm64" ]; then
  echo "Building for arm64 only"
  docker_build_zip arm64
  BUILD_FILE=Dockerfile.alpine.build
  docker_build_zip arm64 -alpine
else
  echo "Building for both amd64 and arm64"
  docker_build_zip amd64
  docker_build_zip arm64
  BUILD_FILE=Dockerfile.alpine.build
  docker_build_zip amd64 -alpine
  docker_build_zip arm64 -alpine
fi
