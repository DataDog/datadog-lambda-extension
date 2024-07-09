#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

set -e

LAYER_DIR=".layers"
VALID_ACCOUNTS=("sandbox" "prod")

publish_layer() {
    region=$1
    layer=$2
    file=$3
    compatible_architectures=$4

    version_nbr=$(aws lambda publish-layer-version --layer-name $layer \
        --description "Datadog Lambda Extension" \
        --compatible-architectures $compatible_architectures \
        --zip-file "fileb://${file}" \
        --region $region \
        | jq -r '.Version'
    )

    permission=$(aws lambda add-layer-version-permission --layer-name $layer \
        --version-number $version_nbr \
        --statement-id "release-$version_nbr" \
        --action lambda:GetLayerVersion \
        --principal "*" \
        --region $region
    )

    echo $version_nbr
}


if [ -z "$ARCHITECTURE "]; then
    printf "[ERROR]: ARCHITECTURE not specified."
    exit 1
fi


if [ -z "$LAYER_FILE" ]; then
    printf "[ERROR]: LAYER_FILE not specified."
    exit 1
fi

LAYER_PATH="${LAYER_DIR}/${LAYER_FILE}"
# Check that the layer files exist
if [ ! -f $LAYER_PATH  ]; then
    printf "[ERROR]: Could not find ${LAYER_PATH}."
    exit 1
fi

if [ "$ARCHITECTURE" == "amd64" ]; then
    LAYER_NAME="Datadog-Extension"
else
    LAYER_NAME="Datadog-Extension-ARM"
fi

if [ -z "$LAYER_NAME" ]; then
    printf "[ERROR]: LAYER_NAME not specified."
    exit 1
fi

AVAILABLE_REGIONS=$(aws ec2 describe-regions | jq -r '.[] | .[] | .RegionName')

if [ -z "$REGION" ]; then
    printf "[ERROR]: REGION not specified."
    exit 1
else
    echo "Region specified: $REGION"
    if [[ ! "$AVAILABLE_REGIONS" == *"$REGION"* ]]; then
        printf "Could not find $REGION in available regions: $AVAILABLE_REGIONS"
        exit 1
    fi
fi

if [ -z "$STAGE" ]; then
    printf "[ERROR]: STAGE not specified.\n"
    exit 1
fi

if [[ "$STAGE" =~ ^(staging|sandbox)$ ]]; then
    # Deploy latest version
    latest_version=$(aws lambda list-layer-versions --region $REGION --layer-name $LAYER_NAME --query 'LayerVersions[0].Version || `0`')
    VERSION=$(($latest_version + 1))
else
    # Running on prod
    if [ -z "$CI_COMMIT_TAG" ]; then
        printf "[Error] No CI_COMMIT_TAG found.\n"
        printf "Exiting script...\n"
        exit 1
    else
        printf "Tag found in environment: $CI_COMMIT_TAG\n"
    fi

    VERSION=$(echo "${CI_COMMIT_TAG##*v}" | cut -d. -f2)
fi

if [ -z "$VERSION" ]; then
    printf "[ERROR]: Layer VERSION not specified"
    exit 1
else
    echo "Layer version parsed: $VERSION"
fi

printf "[$REGION] Starting publishing layers...\n"

# Compatible Architectures
if [ "$ARCHITECTURE" == "amd64" ]; then
    architectures="x86_64"
else
    architectures="arm64"
fi

latest_version=$(aws lambda list-layer-versions --region $REGION --layer-name $LAYER_NAME --query 'LayerVersions[0].Version || `0`')
if [ $latest_version -ge $VERSION ]; then
    printf "[$REGION] Layer $layer version $VERSION already exists in region $REGION, skipping...\n"
    exit 1
elif [ $latest_version -lt $((VERSION-1)) ]; then
    printf "[$REGION][WARNING] The latest version of layer $layer in region $REGION is $latest_version, this will publish all the missing versions including $VERSION\n"
fi

while [ $latest_version -lt $VERSION ]; do
    latest_version=$(publish_layer $REGION $LAYER_NAME $LAYER_PATH $architectures)
    printf "[$REGION] Published version $latest_version for layer $LAYER_NAME in region $REGION\n"

    # This shouldn't happen unless someone manually deleted the latest version, say 28, and
    # then tries to republish 28 again. The published version would actually be 29, because
    # Lambda layers are immutable and AWS will skip deleted version and use the next number.
    if [ $latest_version -gt $VERSION ]; then
        printf "[$REGION] Published version $latest_version is greater than the desired version $VERSION!"
        exit 1
    fi
done

printf "[$REGION] Finished publishing layers...\n"
