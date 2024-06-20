#!/bin/bash

# requires FUNCTION_NAME, REGION, VERSION, and AWS_PROFILE to be set
set -eu pipefail

INVOKES="${INVOKES:-10}"
SLEEP_TIME="${SLEEP_TIME:-1}"

RESPONSE_FILE="response.x"

function update_timeout {
    aws-vault exec "$AWS_PROFILE" -- aws lambda update-function-configuration --region "$REGION" --function-name "$FUNCTION_NAME" --timeout "$1" > /dev/null 2>&1
}

function invoke_and_store_report {
    aws-vault exec "$AWS_PROFILE" -- aws lambda invoke --region "$REGION" --function-name "$FUNCTION_NAME" "$RESPONSE_FILE" --log-type Tail | jq -r '.LogResult' | base64 -d | grep -e '^REPORT ' | tee -a "reports$VERSION.log"
}

true > "reports$VERSION.log"

for i in $(seq "$INVOKES"); do
    update_timeout $((60 + i % 2))
    echo -n "$i: "
    invoke_and_store_report

    sleep "$SLEEP_TIME"
done

sleep "$SLEEP_TIME"

true > "goinit$VERSION.log"

(cd deploy && aws-vault exec "$AWS_PROFILE" -- sls logs -f pierre-bench | grep -e 'init github.com/DataDog/datadog-agent/pkg/serverless/trace @' | tee -a "../goinit$VERSION.log")

echo
python report_stats.py "reports$VERSION.log" "goinit$VERSION.log"