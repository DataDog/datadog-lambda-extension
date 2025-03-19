#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

set -e


if [ -z "$ADD_LAYER_VERSION_PERMISSIONS" ]; then
    printf "[ERROR]: ADD_LAYER_VERSION_PERMISSIONS not specified."
    exit 1
fi

if [ -z "$AUTOMATICALLY_BUMP_VERSION" ]; then
    printf "[ERROR]: AUTOMATICALLY_BUMP_VERSION not specified."
    exit 1
fi

if [ -z "$ARCHITECTURE" ]; then
    printf "[ERROR]: ARCHITECTURE not specified."
    exit 1
fi

if [ -z "${LAYER_NAME_BASE_SUFFIX+x}" ]; then
    printf "[ERROR]: LAYER_NAME_BASE_SUFFIX not specified."
    exit 1
fi

if [ -z "$LAYER_FILE" ]; then
    printf "[ERROR]: LAYER_FILE not specified."
    exit 1
fi


LAYER_DIR=".layers"

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

    # Add permissions only for prod
    if [ "$ADD_LAYER_VERSION_PERMISSIONS" = "1" ]; then
        permission=$(aws lambda add-layer-version-permission --layer-name $layer \
            --version-number $version_nbr \
            --statement-id "release-$version_nbr" \
            --action lambda:GetLayerVersion \
            --principal "*" \
            --region $region
        )
    fi

    echo $version_nbr
}



LAYER_PATH="${LAYER_DIR}/${LAYER_FILE}"
# Check that the layer files exist
if [ ! -f $LAYER_PATH  ]; then
    printf "[ERROR]: Could not find ${LAYER_PATH}."
    exit 1
fi

LAYER_NAME="Datadog-Extension${LAYER_NAME_BASE_SUFFIX}"

if [ -z "$PIPELINE_LAYER_SUFFIX" ]; then
    printf "[$REGION] Deploying layers without suffix\n"
else
    printf "[$REGION] Deploying layers with specified suffix: ${PIPELINE_LAYER_SUFFIX}\n"
    LAYER_NAME="${LAYER_NAME}-${PIPELINE_LAYER_SUFFIX}"
fi

AVAILABLE_REGIONS=$(aws ec2 describe-regions | jq -r '.[] | .[] | .RegionName')

if [ -z "$REGION" ]; then
    printf "[ERROR]: REGION not specified."
    exit 1
else
    printf "Region specified: $REGION\n"
    if [[ ! "$AVAILABLE_REGIONS" == *"$REGION"* ]]; then
        printf "Could not find $REGION in available regions: $AVAILABLE_REGIONS"
        exit 1
    fi
fi

printf "[$REGION] Starting publishing layers...\n"

if [ "$AUTOMATICALLY_BUMP_VERSION" = "1" ]; then
    latest_version=$(aws lambda list-layer-versions --region $REGION --layer-name $LAYER_NAME --query 'LayerVersions[0].Version || `0`')
    VERSION=$(($latest_version + 1))

else
    if [ -z "$CI_COMMIT_TAG" ]; then
        if [ -n "$VERSION" ]; then
            printf "VERSION exists so we should be okay to continue\n"
        else
            printf "[ERROR]: No CI_COMMIT_TAG found and VERSION is not nuymeric.\n"
            printf "Exiting script...\n"
            exit 1
        fi
    else
        printf "Tag found in environment: $CI_COMMIT_TAG\n"

        VERSION="${CI_COMMIT_TAG//[!0-9]/}"
    fi
    printf "Version: ${VERSION}\n"
fi

if [ -z "$VERSION" ]; then
    printf "[ERROR]: Layer VERSION not specified"
    exit 1
elif ! [[ "$VERSION" =~ ^[0-9]+$ ]]; then
    printf "[ERROR]: Layer VERSION must be numeric, got '$VERSION'"
    exit 1
else
    printf "Layer version parsed: $VERSION\n"
fi

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
