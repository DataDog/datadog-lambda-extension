#!/bin/bash

# Save the current path
CURRENT_PATH=$(pwd)

# Build the extension
ARCHITECTURE=amd64 VERSION=1 ./scripts/build_binary_and_layer_dockerized.sh

# Move to the local_tests repo
cd ./local_tests

# Copy the newly built extension in the same folder as the Dockerfile
cp ../.layers/datadog_extension-amd64/extensions/datadog-agent .

# Build the recorder extension which will act as a man-in-a-middle to intercept payloads sent to Datadog
cd ../../datadog-agent/test/integration/serverless/recorder-extension
#GOOS=linux GOARCH=amd64 go build -o "$CURRENT_PATH/local_tests/recorder-extension" main.go
cd "$CURRENT_PATH/local_tests"

# Build java
cd java
gradle buildZip
cd ..
# Unzip source
mkdir -p java/out
rm -rf java/out
mkdir -p java/out
unzip "java/build/distributions/hello.zip" -d "java/out"

# Build the image
docker build -t datadog/extension-local-tests --no-cache -f Dockerfile.Java .

