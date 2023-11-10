#!/bin/bash
# ./build-docker-runtime.sh && ./invoke.sh 2

set -e

if [ -z "$RUNTIME" ]; then
  echo "Runtime not specified, using python 39"
  RUNTIME=python
fi

# Determine architecture, M1 requires arm64 while Intel chip requires amd64. If testing with docker on M1, use amd64
if [ -z "$ARCHITECTURE" ]; then
  if [ "$(uname -m)" == "arm64" ]; then
    ARCHITECTURE=arm64
  else
    ARCHITECTURE=amd64
  fi
fi

SCRIPTS_ROOT=../..

# Build the extension
SERVERLESS_INIT=true ARCHITECTURE=$ARCHITECTURE VERSION=1 $SCRIPTS_ROOT/scripts/build_binary_and_layer_dockerized.sh

# Copy the newly built extension in the same folder as the Dockerfile
mkdir -p .container-opt/
cp -r $SCRIPTS_ROOT/.layers/datadog_extension-$ARCHITECTURE/datadog-agent-go .container-opt/

# Build the image
docker build --platform=linux/$ARCHITECTURE -t datadog/extension-local-tests --no-cache -f serverless-init-python.Dockerfile .
