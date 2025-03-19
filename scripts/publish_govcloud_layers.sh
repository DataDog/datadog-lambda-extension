#! /usr/bin/env bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2025 Datadog, Inc.
#
# USAGE: download the layer bundle from the build pipeline in gitlab. Use the
# Download button on the `layer bundle` job. This will be a zip file containing
# all of the required layers. Run this script as follows:
#
# ENVIRONMENT=[us1-staging-fed or us1-fed] [PIPELINE_LAYER_SUFFIX=optional-layer-suffix] [REGIONS=us-gov-west-1] ./scripts/publish_govcloud_layers.sh <layer-bundle.zip>
#
# protip: you can drag the zip file from finder into your terminal to insert
# its path.

set -e

LAYER_PACKAGE=$1

if [ -z "$LAYER_PACKAGE" ]; then
    printf "[ERROR]: layer package not provided\n"
    exit 1
fi

PACKAGE_NAME=$(basename "$LAYER_PACKAGE" .zip)

if [ -z "$ENVIRONMENT" ]; then
    printf "[ERROR]: ENVIRONMENT not specified\n"
    exit 1
fi

if [ "$ENVIRONMENT" = "us1-staging-fed" ]; then
    AWS_VAULT_ROLE=sso-govcloud-us1-staging-fed-power-user

    export ADD_LAYER_VERSION_PERMISSIONS=0
    export AUTOMATICALLY_BUMP_VERSION=1

    if [[ ! "$PACKAGE_NAME" =~ ^datadog_extension-(signed-)?bundle-[0-9]+$ ]]; then
        echo "[ERROR]: Unexpected package name: $PACKAGE_NAME"
        exit 1
    fi

elif [ $ENVIRONMENT = "us1-fed" ]; then
    AWS_VAULT_ROLE=sso-govcloud-us1-fed-engineering

    export ADD_LAYER_VERSION_PERMISSIONS=1
    export AUTOMATICALLY_BUMP_VERSION=0

    if [[ ! "$PACKAGE_NAME" =~ ^datadog_extension-signed-bundle-[0-9]+$ ]]; then
        echo "[ERROR]: Unexpected package name: $PACKAGE_NAME"
        exit 1
    fi

else
    printf "[ERROR]: ENVIRONMENT not supported, must be us1-staging-fed or us1-fed.\n"
    exit 1
fi

TEMP_DIR=$(mktemp -d)
unzip $LAYER_PACKAGE -d $TEMP_DIR
cp -v $TEMP_DIR/$PACKAGE_NAME/*.zip .layers/


AWS_VAULT_PREFIX="aws-vault exec $AWS_VAULT_ROLE --"

echo "Checking that you have access to the GovCloud AWS account"
$AWS_VAULT_PREFIX aws sts get-caller-identity


AVAILABLE_REGIONS=$($AWS_VAULT_PREFIX aws ec2 describe-regions | jq -r '.[] | .[] | .RegionName')

# Determine the target regions
if [ -z "$REGIONS" ]; then
    echo "Region not specified, running for all available regions."
    REGIONS=$AVAILABLE_REGIONS
else
    echo "Region specified: $REGIONS"
    if [[ ! "$AVAILABLE_REGIONS" == *"$REGIONS"* ]]; then
        echo "Could not find $REGIONS in available regions: $AVAILABLE_REGIONS"
        echo ""
        echo "EXITING SCRIPT."
        exit 1
    fi
fi

declare -A flavors

flavors["amd64"]="arch=amd64 suffix=amd64 layer_name_base_suffix="
flavors["arm64"]="arch=arm64 suffix=arm64 layer_name_base_suffix=-ARM"
flavors["amd64-fips"]="arch=amd64 suffix=amd64-fips layer_name_base_suffix=-FIPS"
flavors["arm64-fips"]="arch=arm64 suffix=arm64-fips layer_name_base_suffix=-ARM-FIPS"

for region in $REGIONS
do
    echo "Starting publishing layers for region $region..."

    export REGION=$region

    for flavor in "${!flavors[@]}"; do
        export LAYER_NAME_BASE_SUFFIX=""
        IFS=' ' read -r -a values <<< "${flavors[$flavor]}"
        for value in "${values[@]}"; do
            case $value in
                arch=*) export ARCHITECTURE="${value#arch=}" ;;
                suffix=*) SUFFIX="${value#suffix=}" ;;
                layer_name_base_suffix=*) export LAYER_NAME_BASE_SUFFIX="${value#layer_name_base_suffix=}" ;;
            esac
        done

        export LAYER_FILE="datadog_extension-$SUFFIX.zip"
        echo "Publishing $LAYER_FILE"

        $AWS_VAULT_PREFIX .gitlab/scripts/publish_layers.sh
    done

done

echo "Done !"
