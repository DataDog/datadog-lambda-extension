#!/bin/bash

LOGS_WAIT_SECONDS=20

set -e

sketchesFunctionName="sketches"
metricFunctionName="enhancedMetricTest"
script_utc_start_time=$(date -u +"%Y%m%dT%H%M%S")

cd "./integration_tests"

echo "Building Go binaries"
make


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
    echo "Removing stack for stage : ${stage}"
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

set +e # Don't exit this script if an invocation fails or there's a diff
echo "Invoking ${metricFunctionName} on ${stage}"
LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} \
serverless invoke --stage ${stage} -f ${metricFunctionName}
# need to double check why two invocation are needed
serverless invoke --stage ${stage} -f ${metricFunctionName}

echo "Sleeping $LOGS_WAIT_SECONDS seconds to wait for logs to appear in CloudWatch..."
sleep $LOGS_WAIT_SECONDS

retry_counter=0
while [ $retry_counter -lt 10 ]; do
    raw_logs=$(LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} serverless logs --stage ${stage} -f $sketchesFunctionName --startTime $script_utc_start_time)
    fetch_logs_exit_code=$?
    if [ $fetch_logs_exit_code -eq 1 ]; then
        echo "Retrying fetch logs for $sketchesFunctionName..."
        retry_counter=$(($retry_counter + 1))
        sleep 10
        continue
    fi
    break
done

# Replace invocation-specific data like timestamps and IDs with XXXX to normalize logs across executions
logs=$(
    echo "$raw_logs" |
    grep "\[sketch\]" | \
    perl -p -e "s/(Ts:) ?[0-9]{10}/\1XXXXXXXXXX/g" | \
    perl -p -e "s/(Cnt:) ?[0-9\.e\-]{2,14}/\1XX/g" | \
    perl -p -e "s/(Min:) ?[0-9\.e\-]{2,14}/\1XX/g" | \
    perl -p -e "s/(Max:) ?[0-9\.e\-]{2,14}/\1XX/g" | \
    perl -p -e "s/(Avg:) ?[0-9\.e\-]{2,14}/\1XX/g" | \
    perl -p -e "s/(Sum:) ?[0-9\.e\-]{2,14}/\1XX/g" | \
    perl -p -e "s/(int32{) ?[0-9]{2,10}/\1XX/g" | \
    perl -p -e "s/(datadog-nodev)[0-9]+\.[0-9]+\.[0-9]+/\1X\.X\.X/g" | \
    perl -p -e "s/(datadog_lambda:v)[0-9]+\.[0-9]+\.[0-9]+/\1X\.X\.X/g" | \
    perl -p -e "s/$stage/XXXXXX/g" | \
    sort
)

function_snapshot_path="./snapshots/metrics/${metricFunctionName}"

if [ ! -f $function_snapshot_path ]; then
    # If no snapshot file exists yet, we create one
    echo "Writing logs to $function_snapshot_path because no snapshot exists yet"
    echo "$logs" >$function_snapshot_path
elif [ -n "$UPDATE_SNAPSHOTS" ]; then
    # If $UPDATE_SNAPSHOTS is set to true write the new logs over the current snapshot
    echo "Overwriting log snapshot for $function_snapshot_path"
    echo "$logs" >$function_snapshot_path
else
    # Compare new logs to snapshots
    diff_output=$(echo "$logs" | diff - $function_snapshot_path)
    if [ $? -eq 1 ]; then
        echo "Failed: Mismatch found between new $function_name logs (first) and snapshot (second):"
        echo "$diff_output"
        mismatch_found=true
    else
        echo "Ok: New logs for $function_name match snapshot"
    fi
fi


if [ "$mismatch_found" = true ]; then
    echo "FAILURE: A mismatch between new data and a snapshot was found and printed above."
    exit 1
fi

echo "SUCCESS: No difference found between snapshots and new return values or logs"