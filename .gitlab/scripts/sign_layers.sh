#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2024 Datadog, Inc.
#
# Usage
#
# LAYER_FILE=<file> ./scripts/sign_layers.sh <account>
#
# LAYER_FILE=datadog_extension-amd64.zip ./scripts/sign_layers.sh sandbox

set -e

LAYER_DIR=".layers"
SIGNING_PROFILE_NAME="DatadogLambdaSigningProfile"
VALID_ACCOUNTS=("sandbox" "prod")

if [ -z "$LAYER_FILE" ]; then
    echo "[ERROR]: $LAYER_FILE not specified."
    exit 1
fi

# Check account parameter
if [ -z "$1" ]; then
    echo "[ERROR]: Account parameter not specified."
    exit 1
fi

if [[ ! "${VALID_ACCOUNTS[@]}" =~ $1 ]]; then
    echo "[ERROR]: Account parameter is invalid. Not in `sandbox` or `prod`."
    exit 1
fi

if [ "$1" = "sandbox" ]; then
    REGION="sa-east-1"
    S3_BUCKET_NAME="dd-lambda-signing-bucket-sandbox"
fi
if [ "$1" = "prod" ]; then
    REGION="us-east-1"
    S3_BUCKET_NAME="dd-lambda-signing-bucket"
fi

echo "---"
echo "Signing layer for $LAYER_FILE"

LAYER_LOCAL_PATH="${LAYER_DIR}/${LAYER_FILE}"

# Upload the layer to S3 for signing
echo "---"
echo "Uploading layer to S3 for signing..."

UUID=$(uuidgen)
S3_UNSIGNED_ZIP_KEY="${UUID}.zip"
S3_UNSIGNED_ZIP_URI="s3://${S3_BUCKET_NAME}/${S3_UNSIGNED_ZIP_KEY}"

aws s3 cp $LAYER_LOCAL_PATH $S3_UNSIGNED_ZIP_URI

# Start a signing job
echo "---"
echo "Starting the signing job..."
SIGNING_JOB_ID=$(aws signer start-signing-job \
    --source "s3={bucketName=${S3_BUCKET_NAME},key=${S3_UNSIGNED_ZIP_KEY},version=null}" \
    --destination "s3={bucketName=${S3_BUCKET_NAME}}" \
    --profile-name $SIGNING_PROFILE_NAME \
    --region $REGION \
    | jq -r '.jobId'\
)

# Wait for the signing job to complete
echo "---"
echo "Waiting for the signing job to complete..."
SECONDS_WAITED_SO_FAR=0
while :
do
    sleep 3
    SECONDS_WAITED_SO_FAR=$((SECONDS_WAITED_SO_FAR + 3))

    SIGNING_JOB_DESCRIPTION=$(aws signer describe-signing-job \
        --job-id $SIGNING_JOB_ID \
        --region $REGION\
    )
    SIGNING_JOB_STATUS=$(echo $SIGNING_JOB_DESCRIPTION | jq -r '.status')
    SIGNING_JOB_STATUS_REASON=$(echo $SIGNING_JOB_DESCRIPTION | jq -r '.statusReason')

    echo "---"
    if [ $SIGNING_JOB_STATUS = "Succeeded" ]; then
        echo "Signing job succeeded!"
        break
    fi

    if [ $SIGNING_JOB_STATUS = "Failed" ]; then
        echo "[ERROR]: Signing job failed"
        echo $SIGNING_JOB_STATUS_REASON
        exit 1
    fi

    if [ $SECONDS_WAITED_SO_FAR -ge 60 ]; then
        echo "[ERROR]: Timed out waiting for the signing job to complete"
        exit 1
    fi

    echo "Signing job still in progress..."
done

# Download the signed ZIP, overwriting the original ZIP
echo "---"
echo "Replacing the local layer with the signed layer from S3..."
S3_SIGNED_ZIP_KEY="${SIGNING_JOB_ID}.zip"
S3_SIGNED_ZIP_URI="s3://${S3_BUCKET_NAME}/${S3_SIGNED_ZIP_KEY}"
aws s3 cp $S3_SIGNED_ZIP_URI $LAYER_LOCAL_PATH

# Delete the signed and unsigned ZIPs in S3
echo "Cleaning up the S3 bucket..."
aws s3api delete-object --bucket $S3_BUCKET_NAME --key $S3_UNSIGNED_ZIP_KEY
aws s3api delete-object --bucket $S3_BUCKET_NAME --key $S3_SIGNED_ZIP_KEY


echo "---"
echo "Successfully signed layer ${LAYER_FILE}!"
