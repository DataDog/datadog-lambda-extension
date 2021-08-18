#!/bin/sh

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

set -e

if [ -z "$VERSION" ]; then
    echo "Extension version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
fi

AGENT_PATH="../datadog-agent"
EXTENSION_DIR="extensions"
TARGET_DIR=$(pwd)/$EXTENSION_DIR

# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

# First prepare a folder with only *mod and *sum files to enable Docker caching capabilities
mkdir -p ./scripts/.src ./scripts/.cache
echo "Copy mod files to build a cache"
find $AGENT_PATH -name "go.mod" | cpio -p -dumv ./scripts/.cache/datadog-agent
find $AGENT_PATH -name "go.sum" | cpio -p -dumv ./scripts/.cache/datadog-agent

echo "Compressing all files to speed up docker copy"
tar --exclude=$AGENT_PATH/.git -czf ./scripts/.src/datadog-agent.tgz $AGENT_PATH

docker build -t datadog/lambda-extension:$VERSION \
    -f scripts/Dockerfile.build \
    --build-arg VERSION=$VERSION .

dockerId=$(docker create datadog/lambda-extension:$VERSION)
docker cp $dockerId:/datadog_agent.zip $TARGET_DIR