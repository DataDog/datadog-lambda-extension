#!/bin/bash
set -e

REGION="us-east-1"
LAYER_NAME="Datadog-Bottlecap-Beta-ag"
LAYER_PATH=../../.layers/datadog_bottlecap-amd64.zip

function publish {
  version_nbr=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda publish-layer-version \
      --layer-name "${LAYER_NAME}" \
      --description "Datadog Bottlecap Beta" \
      --zip-file "fileb://${LAYER_PATH}" \
      --region $REGION | jq -r '.Version')
  echo "DONE: Published version $version_nbr of layer $LAYER_NAME to region $REGION"
}

publish