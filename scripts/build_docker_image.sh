#!/bin/bash

# Use with `VERSION=<DESIRED_VERSION> ./build_docker_image.sh`

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

set -e

DOCKER_REPOSITORY_NAME="datadog/lambda-extension"
DOCKERFILE_LOCATION="scripts/Dockerfile"

# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

if [ -z "$VERSION" ]; then
    echo "Version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
fi

unzip .layers/datadog_extension.zip

# Build the image, tagged with the version
echo "Building the Docker image"
docker build extensions \
  -f $DOCKERFILE_LOCATION \
  -t $DOCKER_REPOSITORY_NAME:$VERSION \
  --no-cache

# Also tag the image with :latest
docker tag $DOCKER_REPOSITORY_NAME:$VERSION $DOCKER_REPOSITORY_NAME:latest