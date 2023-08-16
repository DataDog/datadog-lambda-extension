#!/bin/bash

# Use with `VERSION=<DESIRED_VERSION> ./build_docker_image.sh`

# To publish an Alpine image, set ALPINE=true:
# Use with `VERSION=<DESIRED_VERSION> ALPINE=true ./build_docker_image.sh`

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

set -e

DOCKER_REPOSITORY_NAME="datadog/lambda-extension"

# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

if [ -z "$VERSION" ]; then
    echo "Version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
fi

# Build the image, tagged with the version
echo "Building the Docker image, and pushing to Dockerhub"
if [ "$ALPINE" == "true" ]; then
  docker buildx build --platform linux/arm64,linux/amd64 \
    -t $DOCKER_REPOSITORY_NAME:$VERSION-alpine \
    -t $DOCKER_REPOSITORY_NAME:latest-alpine \
    -f ./scripts/Dockerfile.alpine \
    --build-arg EXTENSION_VERSION="${VERSION}" . \
    --push
else
  docker buildx build --platform linux/arm64,linux/amd64 \
    -t $DOCKER_REPOSITORY_NAME:$VERSION \
echo "Building the non-alpine Docker images, and pushing to Dockerhub"
docker buildx build --platform linux/arm64,linux/amd64 \
  -t $DOCKER_REPOSITORY_NAME:$VERSION-alpine \
  -t $DOCKER_REPOSITORY_NAME:latest-alpine \
  -f ./scripts/Dockerfile.alpine \
  --build-arg EXTENSION_VERSION="${VERSION}" . \
  --push

echo "Building the alpine Docker images, and pushing to Dockerhub"
docker buildx build --platform linux/arm64,linux/amd64 \
  -t $DOCKER_REPOSITORY_NAME:$VERSION \
  -t $DOCKER_REPOSITORY_NAME:latest \
  -f ./scripts/Dockerfile \
  --build-arg EXTENSION_VERSION="${VERSION}" . \
  --push  
