#!/bin/bash

set -e

# Determine architecture, M1 requires arm64 while Intel chip requires amd64
if [ `uname -m` == "arm64" ]; then
    LAYER_NAME=Datadog-Python39-ARM
    ARCHITECTURE=arm64
else
    LAYER_NAME=Datadog-Python39
    ARCHITECTURE=amd64
fi

# Save the current path
CURRENT_PATH=$(pwd)

# Build the extension
VERSION=1 ./scripts/build_binary_and_layer_dockerized.sh

# Move to the local_tests repo
cd ./local_tests

# Copy the newly built extension in the same folder as the Dockerfile
cp ../.layers/datadog_extension-$ARCHITECTURE/extensions/datadog-agent .

# Build the recorder extension which will act as a man-in-a-middle to intercept payloads sent to Datadog
cd ../../datadog-agent/test/integration/serverless/recorder-extension
GOOS=linux GOARCH=$ARCHITECTURE go build -o "$CURRENT_PATH/local_tests/recorder-extension" main.go
cd "$CURRENT_PATH/local_tests"

# Get the latest available version
LATEST_AVAILABLE_VERSION=$(aws-vault exec sandbox-account-admin \
-- aws lambda list-layer-versions --layer-name $LAYER_NAME --region sa-east-1 --max-items 1 \
| jq -r ".LayerVersions | .[0] |  .Version")

# If not yet downloaded, download and unzip
PYTHON_LAYER="$CURRENT_PATH/local_tests/layer-$LATEST_AVAILABLE_VERSION.zip"

if test -f "$PYTHON_LAYER"; then
    echo "The layer has already been downloaded, skipping"
else 
    echo "Downloading the latest Python layer (version $LATEST_AVAILABLE_VERSION)"
    URL=$(aws-vault exec sandbox-account-admin \
        -- aws lambda get-layer-version --layer-name $LAYER_NAME --version-number $LATEST_AVAILABLE_VERSION \
        --query Content.Location --region sa-east-1 --output text)
    curl $URL -o "$PYTHON_LAYER"
    rm -rf $CURRENT_PATH/local_tests/META_INF
    rm -rf $CURRENT_PATH/local_tests/python
    unzip "$PYTHON_LAYER"
fi

# Build the image
docker build -t datadog/extension-local-tests --no-cache -f Dockerfile.Python .