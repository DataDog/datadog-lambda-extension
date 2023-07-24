#!/bin/bash

set -e

if [ -z $RUNTIME ]; then
    echo "Runtime not specified, using python 39"
    RUNTIME=python
fi

# Determine architecture, M1 requires arm64 while Intel chip requires amd64
if [ -z "$ARCHITECTURE" ]; then
  if [ `uname -m` == "arm64" ]; then
      ARCHITECTURE=arm64
  else
      ARCHITECTURE=amd64
  fi
fi

case "$RUNTIME" in
    python)
        if [ "$ARCHITECTURE" == "amd64" ]; then
            LAYER_NAME=Datadog-Python39
        else
            LAYER_NAME=Datadog-Python39-ARM
        fi
        DOCKERFILE=Dockerfile.Python
        ;;

    go)
      DOCKERFILE=Dockerfile.Go
      ;;

    java)
      DOCKERFILE=Dockerfile.Java
      LAYER_NAME=dd-trace-java
      ;;

    node)
        LAYER_NAME=Datadog-Node16-x
        DOCKERFILE=Dockerfile.Node
        ;;
esac

# Save the current path
CURRENT_PATH=$(pwd)

# Build the extension
ARCHITECTURE=$ARCHITECTURE VERSION=1 ./scripts/build_binary_and_layer_dockerized.sh


# Move to the local_tests repo
cd ./local_tests

# Copy the newly built extension in the same folder as the Dockerfile
cp ../.layers/datadog_extension-$ARCHITECTURE/extensions/datadog-agent .

# Build the recorder extension which will act as a man-in-a-middle to intercept payloads sent to Datadog
cd ../../datadog-agent/test/integration/serverless/recorder-extension

if [ `uname -o` == "GNU/Linux" ]; then
      CGO_ENABLED=0 GOOS=linux GOARCH=$ARCHITECTURE go build -o "$CURRENT_PATH/local_tests/recorder-extension" main.go
    else
      GOOS=linux GOARCH=$ARCHITECTURE go build -o "$CURRENT_PATH/local_tests/recorder-extension" main.go
fi

cd "$CURRENT_PATH/local_tests"
if [ -z "$LAYER_PATH" ] && [ -n "$LAYER_NAME" ]; then
    # Get the latest available version
    LATEST_AVAILABLE_VERSION=$(aws-vault exec serverless-sandbox-account-admin \
    -- aws lambda list-layer-versions --layer-name $LAYER_NAME --region sa-east-1 --max-items 1 \
    | jq -r ".LayerVersions | .[0] |  .Version")

    # If not yet downloaded, download and unzip
    LAYER="$CURRENT_PATH/local_tests/layer-$LATEST_AVAILABLE_VERSION.zip"

    if test -f "$LAYER"; then
        echo "The layer has already been downloaded, skipping"
    else
        echo "Downloading the latest $RUNTIME layer (version $LATEST_AVAILABLE_VERSION)"
        URL=$(aws-vault exec serverless-sandbox-account-admin \
            -- aws lambda get-layer-version --layer-name $LAYER_NAME --version-number $LATEST_AVAILABLE_VERSION \
            --query Content.Location --region sa-east-1 --output text)
        curl -k $URL -o "$LAYER"
        rm -rf $CURRENT_PATH/local_tests/META_INF
        rm -rf $CURRENT_PATH/local_tests/python
        unzip "$LAYER"
    fi
elif [ -n "$LAYER_PATH" ]; then
    echo "Using $LAYER_PATH instead of fetching from AWS"
    if test -d "$CURRENT_PATH/local_tests/$RUNTIME"; then
        echo "Removing and rebuilding from local path"
        rm -rf $CURRENT_PATH/local_tests/$RUNTIME
    fi
    unzip $LAYER_PATH -d $CURRENT_PATH/local_tests/
fi

# Build the image
docker build --platform=linux/$ARCHITECTURE -t datadog/extension-local-tests --no-cache -f $DOCKERFILE .
