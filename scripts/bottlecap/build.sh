#!/bin/bash
set -e

function zip_extension {
  local ARCH=amd64
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
  cd ..
  mv datadog_bottlecap-$ARCH/datadog_bottlecap-$ARCH.zip .
  du -hs datadog_bottlecap-$ARCH.zip
}

time zip_extension