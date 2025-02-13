#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

# Use with `VERSION=<DESIRED_EXTENSION_VERSION> ./publish_govcloud.sh`
# Only run this after running the normal release in gitlab and downloading the
# signed layers to the `.layers` directory.

set -e

if [ -z "$I_KNOW_WHAT_I_AM_DOING" ]; then
    # This script differs from our most recent updates to the gitlab build
    # pipelines. We are going to fix it, but you can help! Either you know what
    # you are doing and can let this script know, or you could update the
    # script yourself, or at least let us know that you want it to be updated!
    echo "Please set I_KNOW_WHAT_I_AM_DOING to 'true' to run this script"
    exit 1
fi

# Ensure the target extension version is defined
if [ -z "$VERSION" ]; then
    echo "Extension version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
fi

# Move into the root directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

echo "Checking that you have access to the GovCloud AWS account"
aws-vault exec sso-govcloud-us1-fed-engineering -- aws sts get-caller-identity

read -p "Deploy layers to GovCloud AWS (y/n)?" CONT
if [ "$CONT" == "y" ]; then
    echo "Publishing layers to GovCloud AWS regions"
    aws-vault exec sso-govcloud-us1-fed-engineering -- ./scripts/publish_layers.sh
fi

# Open a PR to the documentation repo to automatically bump layer version
VERSION=$VERSION LAYER=datadog-lambda-extension ./scripts/create_documentation_pr.sh

