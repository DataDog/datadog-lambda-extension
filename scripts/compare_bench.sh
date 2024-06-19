#!/bin/bash

set -eu pipefail

# Ensure agent commit is set so we can compare that commit and the previous one
if [ -z $AGENT_COMMIT ]; then
    echo "AGENT_COMMIT not set"
    exit 1
fi

rm -rf scripts/.cache
rm -rf scripts/.src

export AGENT_COMMIT=$AGENT_COMMIT
export INVOKES=30

./scripts/build_publish_deploy_bench.sh

export AGENT_COMMIT=$AGENT_COMMIT^
export INVOKES=30

rm -rf scripts/.cache
rm -rf scripts/.src

echo 
echo 
echo 

# benchmark the previous agent commit
./scripts/build_publish_deploy_bench.sh

