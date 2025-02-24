#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

set -e

DOCKER_TARGET_IMAGE="425362996713.dkr.ecr.us-east-1.amazonaws.com/self-monitoring-lambda-extension"
EXTENSION_DIR=".layers"
IMAGE_TAG="latest"

printf "Authenticating Docker to ECR...\n"
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin 425362996713.dkr.ecr.us-east-1.amazonaws.com

if [ "$ALPINE" = "0" ]; then
    printf "Building image\n"
    TARGET_IMAGE="Dockerfile.extension_image"
else
    printf "Building image for alpine\n"
    TARGET_IMAGE="Dockerfile.extension_image.alpine"
fi

# NOTE: this probably does not work the way that we expect it to, especially
# when suffixes are involved. This is a known bug but we don't really check
# anything other than the basic `self-monitoring-lambda-extension:latest` image
# in our self-monitoring, so it's not a thing we're going to fix right now.
LAYER_NAME="Datadog-Extension"
if [ -z "$WORKFLOW_LAYER_SUFFIX" ]; then
    printf "Building container images tagged without suffix\n"
else
    printf "Building container images tagged with suffix: ${WORKFLOW_LAYER_SUFFIX}\n"
    LAYER_NAME="${LAYER_NAME}-${WORKFLOW_LAYER_SUFFIX}"
fi

# Increment last version
latest_version=$(aws lambda list-layer-versions --region us-east-1 --layer-name $LAYER_NAME --query 'LayerVersions[0].Version || `0`')
VERSION=$(($latest_version + 1))
printf "Tagging container image with version: $VERSION and latest\n"

docker buildx build \
    --platform $PLATFORM \
    -f ./images/Dockerfile.extension_image \
    --build-arg SUFFIX=$SUFFIX \
    --tag "$DOCKER_TARGET_IMAGE:${IMAGE_TAG}${SUFFIX}" \
    --tag "$DOCKER_TARGET_IMAGE:${VERSION}${SUFFIX}" \
    --push .

printf "Image built and pushed to $DOCKER_TARGET_IMAGE:${IMAGE_TAG}${SUFFIX} for ${PLATFORM}\n"
