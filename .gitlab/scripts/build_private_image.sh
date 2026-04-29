#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

set -e

# ECR target for private extension images, used by self-monitoring container runtimes.
# Defaults to the serverless-testing account's datadog-lambda-extension repo.
PRIVATE_IMAGE_ECR_ACCOUNT="${PRIVATE_IMAGE_ECR_ACCOUNT:-093468662994}"
PRIVATE_IMAGE_ECR_REPO="${PRIVATE_IMAGE_ECR_REPO:-datadog-lambda-extension}"
DOCKER_TARGET_IMAGE="${PRIVATE_IMAGE_ECR_ACCOUNT}.dkr.ecr.us-east-1.amazonaws.com/${PRIVATE_IMAGE_ECR_REPO}"
EXTENSION_DIR=".layers"
IMAGE_TAG="latest"

printf "Authenticating Docker to ECR (%s)...\n" "$PRIVATE_IMAGE_ECR_ACCOUNT"
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin "${PRIVATE_IMAGE_ECR_ACCOUNT}.dkr.ecr.us-east-1.amazonaws.com"

LAYER_NAME="Datadog-Extension"
if [ -z "$PIPELINE_LAYER_SUFFIX" ]; then
    printf "Building container images tagged without suffix\n"
else
    printf "Building container images tagged with suffix: ${PIPELINE_LAYER_SUFFIX}\n"
    LAYER_NAME="${LAYER_NAME}-${PIPELINE_LAYER_SUFFIX}"
fi

# Get the latest published layer version to derive the image tag.
# Layers are published in the sandbox account (425362996713), so query there
# regardless of which account we're pushing images to.
SANDBOX_ACCOUNT="425362996713"
latest_version=$(aws lambda list-layer-versions --region us-east-1 --layer-name "arn:aws:lambda:us-east-1:${SANDBOX_ACCOUNT}:layer:${LAYER_NAME}" --query 'LayerVersions[0].Version || `0`')
VERSION=$(($latest_version + 1))
printf "Tagging container image with version: $VERSION and latest\n"

docker buildx build \
    --platform linux/amd64,linux/arm64 \
    -f ./images/Dockerfile.extension_image \
    --build-arg SUFFIX=$SUFFIX \
    --tag "$DOCKER_TARGET_IMAGE:${IMAGE_TAG}${SUFFIX}" \
    --tag "$DOCKER_TARGET_IMAGE:${VERSION}${SUFFIX}" \
    --push .

printf "Image built and pushed to $DOCKER_TARGET_IMAGE:${IMAGE_TAG}${SUFFIX}\n"
