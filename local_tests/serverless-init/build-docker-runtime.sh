#!/bin/bash
# ./build-docker-runtime.sh && ./invoke.sh 2

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

CURRENT_PATH=$(pwd)
SCRIPTS_ROOT=$(dirname "$(dirname "$CURRENT_PATH")")

# Build the extension
SERVERLESS_INIT=true ARCHITECTURE=$ARCHITECTURE VERSION=1 $SCRIPTS_ROOT/scripts/build_binary_and_layer_dockerized.sh

# Copy the newly built extension in the same folder as the Dockerfile
cp $SCRIPTS_ROOT/.layers/datadog_extension-$ARCHITECTURE/extensions/datadog-agent .

# Build the recorder extension which will act as a man-in-a-middle to intercept payloads sent to Datadog
cd $SCRIPTS_ROOT/../datadog-agent/test/integration/serverless

ARCHITECTURE=$ARCHITECTURE ./build_recorder.sh
echo "Extracting recordering-extension executable into $SCRIPTS_ROOT/local_tests"
unzip -j -o ./recorder-extension/ext.zip "extensions/recorder-extension" -d $SCRIPTS_ROOT/local_tests

cd -

# Build the image
docker build --platform=linux/$ARCHITECTURE -t datadog/extension-local-tests --no-cache -f $DOCKERFILE .
