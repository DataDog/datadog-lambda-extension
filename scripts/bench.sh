#!/bin/bash

# requires FUNCTION_NAME, REGION, and AWS_PROFILE to be set
set -eu pipefail

if [ -z "$I_KNOW_WHAT_I_AM_DOING" ]; then
    # This script differs from our most recent updates to the gitlab build
    # pipelines. We are going to fix it, but you can help! Either you know what
    # you are doing and can let this script know, or you could update the
    # script yourself, or at least let us know that you want it to be updated!
    echo "Please set I_KNOW_WHAT_I_AM_DOING to 'true' to run this script"
    exit 1
fi

INVOKES="${INVOKES:-10}"
SLEEP_TIME="${SLEEP_TIME:-1}"

RESPONSE_FILE="response.x"

function update_timeout {
    aws-vault exec "$AWS_PROFILE" -- aws lambda update-function-configuration --region "$REGION" --function-name "$FUNCTION_NAME" --timeout "$1" > /dev/null 2>&1
}

function invoke_and_store_report {
    aws-vault exec "$AWS_PROFILE" -- aws lambda invoke --region "$REGION" --function-name "$FUNCTION_NAME" "$RESPONSE_FILE" --log-type Tail | jq -r '.LogResult' | base64 -d | grep -e '^REPORT ' | tee -a reports.log
}

true > reports.log

for i in $(seq "$INVOKES"); do
    update_timeout $((60 + i % 2))
    echo -n "$i: "
    invoke_and_store_report

    sleep "$SLEEP_TIME"
done

echo
python report_stats.py reports.log
