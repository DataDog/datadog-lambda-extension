#!/bin/bash
set -e

ARCH="amd64"
REGION="us-east-1"
LAYER_NAME="Datadog-Bottlecap-Beta-ag"
LAYER_PATH=../../.layers/datadog_bottlecap-$ARCH.zip

zip_extension() {
  local LAYER_DIR=../../.layers/datadog_bottlecap-$ARCH
  mkdir -p $LAYER_DIR

  docker_id=$(docker create public.ecr.aws/datadog/lambda-extension:57 --platform linux/$ARCH --entrypoint /bin/bash)
  docker cp $docker_id:/opt/extensions/datadog-agent $LAYER_DIR/datadog-agent-go
  docker rm -f $docker_id

  cp ../datadog_wrapper $LAYER_DIR/datadog_wrapper

  mkdir -p $LAYER_DIR/extensions
  cd ../../bottlecap/target/release/
    cargo build --release
  cd -

  cp ../../bottlecap/target/release/bottlecap $LAYER_DIR/extensions/datadog-agent

  cd $LAYER_DIR
    chmod 755 datadog_wrapper datadog-agent-go extensions/datadog-agent
    du -hs datadog_wrapper datadog-agent-go extensions/datadog-agent
    zip -r datadog_bottlecap-$ARCH.zip datadog_wrapper datadog-agent-go extensions/datadog-agent
    mv datadog_bottlecap-$ARCH.zip ..
  cd -

  du -hs $LAYER_DIR/../datadog_bottlecap-$ARCH.zip
}

publish() {
  version_nbr=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda publish-layer-version \
      --layer-name "${LAYER_NAME}" \
      --description "Datadog Bottlecap Beta" \
      --zip-file "fileb://${LAYER_PATH}" \
      --region $REGION | jq -r '.Version')
  echo "$version_nbr"
}

