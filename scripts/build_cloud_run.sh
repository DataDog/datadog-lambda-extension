#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

# Usage: AGENT_VERSION=7.43.0 VERSION=5 ARCHITECTURE=[amd64|arm64] ./scripts/build_cloud_run.sh

# Optional environment variables:
# VERSION - Use a specific version number

set -e

# Move into the root directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

AGENT_VERSION=$AGENT_VERSION VERSION=$VERSION ARCHITECTURE=$ARCHITECTURE CLOUD_RUN=true ./scripts/build_binary_and_layer_dockerized.sh
