#!/bin/bash

LOGS_WAIT_SECONDS=20

sketchesFunctionName="sketches"
metricFunctionName="metricTest"

cd "./integration_tests"


if [ -z "$LAYER_VERSION" ]; then
    echo "LAYER_VERSION not found "
    exit 1
fi

if [ -z "$EXTENSION_VERSION" ]; then
    echo "EXTENSION_VERSION not found "
    exit 1
fi

# random 8-character ID to avoid collisions with other runs
stage=$(xxd -l 4 -c 4 -p < /dev/random)

# always remove the stacks before exiting, no matter what
function remove_stack() {
    echo "Removing stack for stage : ${run_id}"
    LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} \
    serverless remove --stage ${stage} 
}

# making sure the remove_stack function will be called no matter what
trap remove_stack EXIT

# deploying the stack
LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} \
serverless deploy --stage ${stage}

# invoking functions
function_names=("enhancedMetricTest")

echo "Invoking ${metricFunctionName} on ${stage}"
LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} \
serverless invoke --stage ${stage} -f ${metricFunctionName}

echo "Sleeping $LOGS_WAIT_SECONDS seconds to wait for logs to appear in CloudWatch..."
sleep $LOGS_WAIT_SECONDS

retry_counter=0
while [ $retry_counter -lt 10 ]; do
    raw_logs=$(LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} serverless logs --stage ${stage} -f $sketchesFunctionName)
    fetch_logs_exit_code=$?
    if [ $fetch_logs_exit_code -eq 1 ]; then
        echo "Retrying fetch logs for $sketchesFunctionName..."
        retry_counter=$(($retry_counter + 1))
        sleep 10
        continue
    fi
    break
done

function_snapshot_path="./snapshots/metrics/${metricFunctionName}"

echo $function_snapshot_path

echo $raw_logs > file
cat file

diff_output=$(cat file | grep "sketches" | diff -w - <(sort $function_snapshot_path))
if [ $? -eq 1 ]; then
    echo "Failed: Mismatch found between new $function_name logs (first) and snapshot (second):"
    echo "$diff_output"
    mismatch_found=true
else
    echo "Ok: New logs for $function_name match snapshot"
fi

if [ "$mismatch_found" = true ]; then
    echo "FAILURE: A mismatch between new data and a snapshot was found and printed above."
    exit 1
fi

echo "SUCCESS: No difference found between snapshots and new return values or logs"