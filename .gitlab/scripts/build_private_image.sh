#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

set -e

DOCKER_TARGET_IMAGE="425362996713.dkr.ecr.us-east-1.amazonaws.com/self-monitoring-lambda-extension"
EXTENSION_DIR=".layers"
IMAGE_TAG="latest"

if [ -z "$CI_COMMIT_TAG" ]; then
    # Running on dev
    printf "Running on dev environment\n"
    VERSION="dev"
else
    printf "Found version tag in environment\n"
    VERSION=$(echo "${CI_COMMIT_TAG##*v}" | cut -d. -f2)
    printf "Version: $VERSION\n"
fi

printf "Authenticating Docker to ECR...\n"
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin 425362996713.dkr.ecr.us-east-1.amazonaws.com

if [ -z "$ALPINE" ]; then
    printf "Building image\n"
    TARGET_IMAGE="Dockerfile"
else
    printf "Building image for alpine\n"
    TARGET_IMAGE="Dockerfile.alpine"
    BUILD_SUFFIX="-alpine"
fi

docker buildx build \
    --platform linux/amd64,linux/arm64 \
    -f ./scripts/${TARGET_IMAGE} \
    --tag "$DOCKER_TARGET_IMAGE:${IMAGE_TAG}${BUILD_SUFFIX}" \
    --push .

printf "Image built and pushed to $DOCKER_TARGET_IMAGE:${IMAGE_TAG}${BUILD_SUFFIX} for arm64 and amd64\n"
