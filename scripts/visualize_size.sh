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
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $DIR/..

./serverless/build_binary_and_layer.sh
cd ./extensions/

echo "Analyzing go binary"
go tool nm -size datadog-agent | c++filt > ../go-binsize-viz/agent.txt
cd ../go-binsize-viz/

echo "Converting data to json"
python3 tab2pydic.py agent.txt > out.py
python3 simplify.py out.py > data.js

echo "Open http://localhost:8000/treemap_v3.html"
python3 -m http.server