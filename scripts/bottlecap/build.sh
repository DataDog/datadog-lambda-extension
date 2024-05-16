#!/bin/bash
set -e

function zip_extension {
  local LAYER_DIR=../../.layers/datadog_bottlecap-amd64
  mkdir -p $LAYER_DIR

  docker_id=$(docker create public.ecr.aws/datadog/lambda-extension:57 --entrypoint /bin/bash)
  docker cp $docker_id:/opt/extensions/datadog-agent $LAYER_DIR/datadog-agent-go
  docker rm -f $docker_id

  cp ../datadog_wrapper $LAYER_DIR/datadog_wrapper

  mkdir -p $LAYER_DIR/extensions
  cp ../../bottlecap/bottlecap-main $LAYER_DIR/extensions/datadog-agent
  cd $LAYER_DIR/..
  zip -r datadog_bottlecap-amd64.zip datadog_bottlecap-amd64
}

zip_extension