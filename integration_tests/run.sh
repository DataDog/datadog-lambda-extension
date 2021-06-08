#!/bin/bash

LOGS_WAIT_SECONDS=30

set -e

script_utc_start_time=$(date -u +"%Y%m%dT%H%M%S")

cd "./integration_tests"

#zip extension
cd recorder-extension
zip -rq ext.zip extensions -x ".*" -x "__MACOSX" -x "extensions/.*"
cd ..

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
stage="12341234"
# always remove the stacks before exiting, no matter what
function remove_stack() {
    echo "Removing stack for stage : ${stage}"
    LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} \
    serverless remove --stage ${stage} 
}

# making sure the remove_stack function will be called no matter what
# trap remove_stack EXIT

# deploying the stack
LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} \
serverless deploy --stage ${stage}

# invoking functions
metric_function_names=("enhancedMetricTest" "noEnhancedMetricTest" "timeoutMetricTest")
log_function_names=("logTest")

all_functions=("${metric_function_names[@]}" "${log_function_names[@]}")

set +e # Don't exit this script if an invocation fails or there's a diff

for function_name in "${all_functions[@]}"; do
    LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} \
    serverless invoke --stage ${stage} -f ${function_name}
    # two invocations are needed since enhanced metrics are computed with the REPORT log line (which is trigered at the end of the first invocation)
    return_value=$(serverless invoke --stage ${stage} -f ${function_name})

    # Compare new return value to snapshot
    diff_output=$(echo "$return_value" | diff - "./snapshots/expectedInvocationResult")
    if [ $? -eq 1 ]; then
        echo "Failed: Return value for $function_name does not match snapshot:"
        echo "$diff_output"
        mismatch_found=true
    else
        echo "Ok: Return value for $function_name matches snapshot"
    fi
done

echo "Sleeping $LOGS_WAIT_SECONDS seconds to wait for logs to appear in CloudWatch..."
sleep $LOGS_WAIT_SECONDS

for function_name in "${all_functions[@]}"; do
    echo "Fetching logs for ${function_name} on ${stage}"
    retry_counter=0
    while [ $retry_counter -lt 10 ]; do
        raw_logs=$(LAYER_VERSION=${LAYER_VERSION} EXTENSION_VERSION=${EXTENSION_VERSION} serverless logs --stage ${stage} -f $function_name --startTime $script_utc_start_time)
        fetch_logs_exit_code=$?
        if [ $fetch_logs_exit_code -eq 1 ]; then
            echo "Retrying fetch logs for $sketchesFunctionName..."
            retry_counter=$(($retry_counter + 1))
            sleep 10
            continue
        fi
        break
    done

    if [[ " ${metric_function_names[@]} " =~ " ${function_name} " ]]; then
        # Replace invocation-specific data like timestamps and IDs with XXXX to normalize logs across executions
        logs=$(
            echo "$raw_logs" | \
            grep "\[sketch\]" | \
            perl -p -e "s/(ts\":\")[0-9]{10}/\1XXX/g" | \
            perl -p -e "s/(min\":)[0-9\.]{2,20}/\1XXX/g" | \
            perl -p -e "s/(max\":)[0-9\.]{2,20}/\1XXX/g" | \
            perl -p -e "s/(cnt\":)[0-9\.]{2,20}/\1XXX/g" | \
            perl -p -e "s/(avg\":)[0-9\.]{2,20}/\1XXX/g" | \
            perl -p -e "s/(sum\":)[0-9\.]{2,20}/\1XXX/g" | \
            perl -p -e "s/(k\":\[)[0-9\.]{1,20}/\1XXX/g" | \
            perl -p -e "s/(datadog-nodev)[0-9]+\.[0-9]+\.[0-9]+/\1X\.X\.X/g" | \
            perl -p -e "s/(datadog_lambda:v)[0-9]+\.[0-9]+\.[0-9]+/\1X\.X\.X/g" | \
            perl -p -e "s/$stage/XXXXXX/g" | \
            sort
        )
        echo $raw_logs > raw_toto_$function_name
        echo $logs > toto_$function_name
    elif [[ " ${log_function_names[@]} " =~ " ${function_name} " ]]; then
        logs=$(
            echo "$raw_logs" | \
            grep "\[log\]" | \
            perl -p -e "s/(timestamp\":)[0-9]{13}/\1XXX/g" | \
            perl -p -e "s/(ddtags\":\")[a-zA-Z0-9\:\-,_]+/\1XXX/g" | \
            perl -p -e "s/(\"REPORT |START |END |HTTP ).*/\1XXX\"}}/g" | \
            perl -p -e "s/(request_id\":\")[a-zA-Z0-9\-,]+/\1XXX/g"| \
            perl -p -e "s/$stage/XXXXXX/g" | \
            perl -p -e "s/(\"message\":\").*(XXX LOG)/\1\2\3/g"
        )
    else #traces are not yet integration-tested
        logs=$(
            echo "$raw_logs"
        )
    fi

    function_snapshot_path="./snapshots/${function_name}"

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

done

if [ "$mismatch_found" = true ]; then
    echo "FAILURE: A mismatch between new data and a snapshot was found and printed above."
    exit 1
fi

echo "SUCCESS: No difference found between snapshots and new return values or logs"