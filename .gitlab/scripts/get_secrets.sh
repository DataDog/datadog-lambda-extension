#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.

set -e

if [ -z "$EXTERNAL_ID_NAME" ]; then
    printf "[Error] No EXTERNAL_ID_NAME found.\n"
    printf "Exiting script...\n"
    exit 1
fi

if [ -z "$ROLE_TO_ASSUME" ]; then
    printf "[Error] No ROLE_TO_ASSUME found.\n"
    printf "Exiting script...\n"
    exit 1
fi

printf "Getting AWS External ID...\n"

EXTERNAL_ID=$(aws ssm get-parameter \
    --region us-east-1 \
    --name "ci.datadog-lambda-extension.$EXTERNAL_ID_NAME" \
    --with-decryption \
    --query "Parameter.Value" \
    --out text)

printf "Getting DD API KEY...\n"

export DD_API_KEY=$(aws ssm get-parameter \
    --region us-east-1 \
    --name ci.datadog-lambda-extension.dd-api-key \
    --with-decryption \
    --query "Parameter.Value" \
    --out text)

printf "Assuming role...\n"

export $(printf "AWS_ACCESS_KEY_ID=%s AWS_SECRET_ACCESS_KEY=%s AWS_SESSION_TOKEN=%s" \
    $(aws sts assume-role \
    --role-arn "arn:aws:iam::$AWS_ACCOUNT:role/$ROLE_TO_ASSUME"  \
    --role-session-name "ci.datadog-lambda-extension-$CI_JOB_ID-$CI_JOB_STAGE" \
    --query "Credentials.[AccessKeyId,SecretAccessKey,SessionToken]" \
    --external-id $EXTERNAL_ID \
    --output text))
