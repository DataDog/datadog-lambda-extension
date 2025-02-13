#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

# Copy layers from us-east-1 to new region
# args: [new-region]

set -e

if [ -z "$I_KNOW_WHAT_I_AM_DOING" ]; then
    # This script differs from our most recent updates to the gitlab build
    # pipelines. We are going to fix it, but you can help! Either you know what
    # you are doing and can let this script know, or you could update the
    # script yourself, or at least let us know that you want it to be updated!
    echo "Please set I_KNOW_WHAT_I_AM_DOING to 'true' to run this script"
    exit 1
fi

FROM_REGION='us-east-1'

LAYER_NAMES=("Datadog-Extension")

NEW_REGION=$1

get_max_version() {
    layer_name=$1
    region=$2
    last_layer_version=$(aws lambda list-layer-versions --layer-name $layer_name --region $region | jq -r ".LayerVersions | .[0] |  .Version")
    if [ "$last_layer_version" == "null" ]; then
        echo 0
    else
        echo $last_layer_version
    fi
}

if [ -z "$1" ]; then
    echo "Region parameter not specified, exiting"
    exit 1
fi

for layer_name in "${LAYER_NAMES[@]}"; do
    # get latest version
    last_layer_version=$(get_max_version $layer_name $FROM_REGION)
    starting_version=$(get_max_version $layer_name $NEW_REGION)
    starting_version=$(expr $starting_version + 1)

    # exit if region is already all caught up
    if [ $starting_version -ge $last_layer_version ]; then
        echo "INFO: $NEW_REGION is already up to date for $layer_name"
        continue
    fi

    # run for each version of layer
    for i in $(seq 1 $last_layer_version); do
        layer_path=$layer_name"_"$i.zip

        # download layer versions
        URL=$(AWS_REGION=$FROM_REGION aws lambda get-layer-version --layer-name $layer_name --version-number $i --query Content.Location --output text)
        curl $URL -o $layer_path

        # publish layer to new region
        ./publish_layer

        publish_layer $NEW_REGION
        rm $layer_path
    done
done
