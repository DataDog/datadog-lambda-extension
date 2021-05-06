#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2020 Datadog, Inc.

# Use with `VERSION=<DESIRED_VERSION> ./build_docker_image.sh`

set -e

DOCKER_REPOSITORY_NAME="datadog/lambda-extension"

# Ensure the target extension version is defined
if [ -z "$VERSION" ]; then
    echo "Extension version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
fi

# Ensure we are releasing the correct commit
cd ~/dd/datadog-agent
if [[ `git status --porcelain` ]]; then
    echo "Detected uncommitted changes on datadog-agent branch, aborting"
    exit 1
fi

CURRENT_SHA=$(git rev-parse HEAD)
COMMIT_MESSAGE=$(git log -1 --pretty=%B)
echo "Current commit: $CURRENT_SHA"
echo "Current commit message: $COMMIT_MESSAGE"
echo
read -p "Ready to publish commit $CURRENT_SHA as Datadog Lambda Extension version $VERSION (y/n)?" CONT
if [ "$CONT" != "y" ]; then
    echo "Exiting"
    exit 1
fi
cd -

# Move into the script directory
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $DIR

# Manually confirm access
read -p "Please confirm that you have write access to the datadog-agent repository in GitHub. (y/n)" CONT
if [ "$CONT" != "y" ]; then
    echo "Exiting"
    exit 1
fi
read -p "Please confirm that you have push access to the datadog/lambda-extension repository in Dockerhub. (y/n)" CONT
if [ "$CONT" != "y" ]; then
    echo "Exiting"
    exit 1
fi

docker login

echo "Checking that you have access to the commercial AWS account"
aws-vault exec prod-engineering -- aws sts get-caller-identity

echo "Checking that you have access to the GovCloud AWS account"
saml2aws login -a govcloud-us1-fed-human-engineering
AWS_PROFILE=govcloud-us1-fed-human-engineering aws sts get-caller-identity

COMPRESS=true ./build_binary_and_layer.sh
./build_docker_image.sh

echo "Signing the layer"
aws-vault exec prod-engineering -- ./sign_layers.sh prod

echo "Publishing layers to commercial AWS regions"
aws-vault exec prod-engineering --no-session -- ./publish_layers.sh

echo "Publishing layers to GovCloud AWS regions"
saml2aws login -a govcloud-us1-fed-human-engineering
AWS_PROFILE=govcloud-us1-fed-human-engineering ./publish_layers.sh

echo "Pushing Docker image to Dockerhub"
docker push $DOCKER_REPOSITORY_NAME:$VERSION
docker push $DOCKER_REPOSITORY_NAME:latest

echo "Creating tag for release on GitHub"
git tag "lambda-extension-$VERSION"
git push origin "refs/tags/lambda-extension-$VERSION"

echo "New extension version published to AWS and Dockerhub!"
echo
echo "Please create a new GitHub release with the tag lambda-extension-${VERSION}"
echo "https://github.com/DataDog/datadog-agent/releases/new?tag=lambda-extension-$VERSION&title=lambda-extension-$VERSION"