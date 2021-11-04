#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2020 Datadog, Inc.

# Publish the datadog lambda layer across regions, using the AWS CLI
# Usage: VERSION=5 REGIONS=us-east-1 publish_layers.sh
# VERSION is required.
set -e

# Move into the root directory, so this script can be called from any directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR/..

LAYER_PATHS=(".layers/datadog_extension-amd64.zip")
LAYER_NAMES=("Datadog-Extension")
AVAILABLE_REGIONS=$(aws ec2 describe-regions | jq -r '.[] | .[] | .RegionName')

# Check that the layer files exist
for layer_file in "${LAYER_PATHS[@]}"
do
    if [ ! -f $layer_file  ]; then
        echo "Could not find $layer_file."
        exit 1
    fi
done

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

# Determine the target layers
if [ -z "$LAYERS" ]; then
    echo "Layer not specified, running for all layers."
    LAYERS=("${LAYER_NAMES[@]}")
else
    echo "Layer specified: $LAYERS"
    if [[ ! " ${LAYER_NAMES[@]} " =~ " ${LAYERS} " ]]; then
        echo "Could not find $LAYERS in available layers: ${LAYER_NAMES[@]}"
        echo ""
        echo "EXITING SCRIPT."
        exit 1
    fi
fi

# Determine the target layer version
if [ -z "$VERSION" ]; then
    echo "Layer version not specified"
    echo ""
    echo "EXITING SCRIPT."
    exit 1
else
    echo "Layer version specified: $VERSION"
fi

read -p "Ready to publish layer $LAYER_NAME version $VERSION to regions ${REGIONS[*]} (y/n)?" CONT
if [ "$CONT" != "y" ]; then
    echo "Exiting"
    exit 1
fi

index_of_layer() {
    layer_name=$1
    for i in "${!LAYER_NAMES[@]}"; do
        if [[ "${LAYER_NAMES[$i]}" = "${layer_name}" ]]; then
            echo "${i}";
        fi
    done
}

publish_layer() {
    region=$1
    layer=$2
    file=$3
    version_nbr=$(aws lambda publish-layer-version --layer-name "${layer}" \
        --description "Datadog Lambda Extension" \
        --zip-file "fileb://${file}" \
        --region $region | jq -r '.Version')

    permission=$(aws lambda add-layer-version-permission --layer-name $layer \
        --version-number $version_nbr \
        --statement-id "release-$version_nbr" \
        --action lambda:GetLayerVersion --principal "*" \
        --region $region)

    echo $version_nbr
}

for region in $REGIONS
do
    echo "Starting publishing layers for region $region..."
    for layer_name in "${LAYERS[@]}"; do
        latest_version=$(aws lambda list-layer-versions --region $region --layer-name "${layer_name}" --query 'LayerVersions[0].Version || `0`')
        if [ $latest_version -ge $VERSION ]; then
            echo "Layer $layer_name  version $VERSION already exists in region $region, skipping..."
            continue
        elif [ $latest_version -lt $((VERSION-1)) ]; then
            read -p "WARNING: The latest version of layer $layer_name in region $region is $latest_version, publish all the missing versions including $VERSION or EXIT the script (y/n)?" CONT
            if [ "$CONT" != "y" ]; then
                echo "Exiting"
                exit 1
            fi
        fi
        index=$(index_of_layer $layer_name)
        layer_path="${LAYER_PATHS[$index]}"
        while [ $latest_version -lt $VERSION ]; do
            echo $layer_path
            latest_version=$(publish_layer $region $layer_name $layer_path)
            echo "Published version $latest_version for layer $layer_name in region $region"

            # This shouldn't happen unless someone manually deleted the latest version, say 28, and
            # then tries to republish 28 again. The published version would actually be 29, because
            # Lambda layers are immutable and AWS will skip deleted version and use the next number. 
            if [ $latest_version -gt $VERSION ]; then
                echo "ERROR: Published version $latest_version is greater than the desired version $VERSION!"
                echo "Exiting"
                exit 1
            fi
        done
    done
done

echo "Done !"
