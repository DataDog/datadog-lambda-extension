#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2021 Datadog, Inc.

#!/bin/bash
set -e

# Move into the scripts directory
SCRIPTS_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $SCRIPTS_DIR

./build_binary_and_layer_dockerized.sh
REGIONS=ap-southeast-2 aws-vault exec sandbox-account-admin -- ./publish_layers.sh
