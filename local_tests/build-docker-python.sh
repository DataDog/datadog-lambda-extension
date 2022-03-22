#!/bin/bash

set -e

# Save the current path
CURRENT_PATH=$(pwd)

# Build the extension
ARCHITECTURE=amd64 VERSION=1 AGENT_VERSION=$AGENT_VERSION ./scripts/build_binary_and_layer_dockerized.sh

# Move to the local_tests repo
cd ./local_tests

# Copy the newly built extension in the same folder as the Dockerfile
cp ../.layers/datadog_extension-amd64/extensions/datadog-agent .

# Build the recorder extension which will act as a man-in-a-middle to intercept payloads sent to Datadog
cd ../../datadog-agent/test/integration/serverless/recorder-extension
GOOS=linux GOARCH=amd64 go build -o "$CURRENT_PATH/local_tests/recorder-extension" main.go
cd "$CURRENT_PATH/local_tests"

# Get the latest available version
LATEST_AVAILABLE_VERSION=$(aws-vault exec sandbox-account-admin \
-- aws lambda list-layer-versions --layer-name Datadog-Python39 --region sa-east-1 --max-items 1 \
| jq -r ".LayerVersions | .[0] |  .Version")

# If not yet downloaded, download and unzip
PYTHON_LAYER="$CURRENT_PATH/local_tests/layer-$LATEST_AVAILABLE_VERSION.zip"

if test -f "$PYTHON_LAYER"; then
    echo "The layer has already been downloaded, skipping"
else 
    echo "Downloading the latest Python layer (version $LATEST_AVAILABLE_VERSION)"
    URL=$(aws-vault exec sandbox-account-admin \
        -- aws lambda get-layer-version --layer-name Datadog-Python39 --version-number $LATEST_AVAILABLE_VERSION \
        --query Content.Location --region sa-east-1 --output text)
    curl $URL -o "$PYTHON_LAYER"
    rm -rf $CURRENT_PATH/local_tests/META_INF
    rm -rf $CURRENT_PATH/local_tests/python
    unzip "$PYTHON_LAYER"
fi

# Build the image
docker build -t datadog/extension-local-tests --no-cache -f Dockerfile.Python .