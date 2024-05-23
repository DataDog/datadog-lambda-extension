#!/bin/bash
set -e

ARCH="amd64"
REGION="us-east-1"
LAYER_NAME="Datadog-Bottlecap-Beta-ag"
LAYER_PATH=../../.layers/datadog_bottlecap-$ARCH.zip

publish() {
  version_nbr=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda publish-layer-version \
      --layer-name "${LAYER_NAME}" \
      --description "Datadog Bottlecap Beta" \
      --zip-file "fileb://${LAYER_PATH}" \
      --region $REGION | jq -r '.Version')
  echo "$version_nbr"
}
