#!/bin/bash

set -eu pipefail

# Ensure agent commit is set so we can compare that commit and the previous one
if [ -z $AGENT_COMMIT ]; then
    echo "AGENT_COMMIT not set"
    exit 1
fi

# Ensure the function has a idenifiable name
if [ -z $NAME ]; then
    echo "NAME not set"
    exit 1
fi

rm -rf scripts/.cache
rm -rf scripts/.src

export AGENT_COMMIT=$AGENT_COMMIT
export FUNCTION_NAME="Without-$NAME"
export INVOKES=100

./scripts/build_publish_deploy_bench.sh

export FUNCTION_NAME="With-$NAME"
export AGENT_COMMIT=$AGENT_COMMIT^
export INVOKES=100

rm -rf scripts/.cache
rm -rf scripts/.src

echo 
echo 
echo 

# benchmark the previous agent commit
./scripts/build_publish_deploy_bench.sh

