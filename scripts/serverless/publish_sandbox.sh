#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2020 Datadog, Inc.

#!/bin/bash
set -e

# Move into the script directory
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd $DIR

./build_binary_and_layer.sh
aws-vault exec sandbox-account-admin -- ./sign_layers.sh sandbox
REGIONS=sa-east-1 aws-vault exec sandbox-account-admin -- ./publish_layers.sh
