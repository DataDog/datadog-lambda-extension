#!/bin/bash
# ./local_tests/serverless-init/build-docker-runtime.sh && ./local_tests/invoke.sh 2

set -e

if [ -z $RUNTIME ]; then
  echo "Runtime not specified, using python 39"
  RUNTIME=python
fi

# Determine architecture, M1 requires arm64 while Intel chip requires amd64. If testing with docker on M1, use amd64
if [ -z "$ARCHITECTURE" ]; then
  if [ $(uname -m) == "arm64" ]; then
    ARCHITECTURE=arm64
  else
    ARCHITECTURE=amd64
  fi
fi

DOCKERFILE=serverless-init-python.Dockerfile

# Save the current path
CURRENT_PATH=$(pwd)

# Build the extension
SERVERLESS_INIT=true ARCHITECTURE=$ARCHITECTURE VERSION=1 ./scripts/build_binary_and_layer_dockerized.sh

# Move to the local_tests repo
cd ./local_tests/serverless-init

# Copy the newly built extension in the same folder as the Dockerfile
cp ../../.layers/datadog_extension-$ARCHITECTURE/extensions/datadog-agent .

# Build the recorder extension which will act as a man-in-a-middle to intercept payloads sent to Datadog
cd ../../../datadog-agent/test/integration/serverless/recorder-extension

if [ $(uname -o) == "GNU/Linux" ]; then
  CGO_ENABLED=0 GOOS=linux GOARCH=$ARCHITECTURE go build -o "$CURRENT_PATH/local_tests/recorder-extension" main.go
else
  GOOS=linux GOARCH=$ARCHITECTURE go build -o "$CURRENT_PATH/local_tests/recorder-extension" main.go
fi

cd "$CURRENT_PATH/local_tests"

# Build the image
docker build --platform=linux/$ARCHITECTURE -t datadog/extension-local-tests --no-cache -f serverless-init/$DOCKERFILE .
