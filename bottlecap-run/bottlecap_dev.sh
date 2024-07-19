#!/bin/bash
# example usage
# LAMBDA_NAME=svls4825m-bottlecap-busy-node20-lambda LAYER_NAME=Datadog-Bottlecap-Beta-ag  ARCH=amd64 ./bottlecap_dev.sh all
set -e

_REGION="us-east-1"

_require_argument() {
  local arg_name=${1}
  local arg_value=${!1}
  if [ -z "$arg_value" ]; then
      echo "$arg_name not specified. Exiting script"
      exit 1
  fi
}

build_local() {
  # if compiled with modern distro, glibc 2.26 will be used, which is not compatible with Amazon Linux 1 (in node16 runtime et sim)
  _require_argument ARCH
  local _LAYER_DIR=../.layers/datadog_bottlecap-$ARCH
  mkdir -p $_LAYER_DIR

  docker_id=$(docker create public.ecr.aws/datadog/lambda-extension:57 --platform linux/$ARCH --entrypoint /bin/bash)
  docker cp $docker_id:/opt/extensions/datadog-agent $_LAYER_DIR/datadog-agent-go
  docker rm -f $docker_id

  cp ../scripts/datadog_wrapper $_LAYER_DIR/datadog_wrapper

  mkdir -p $_LAYER_DIR/extensions
  cd ../bottlecap/target/release/
    cargo build --release
  cd -

  cp ../bottlecap/target/release/bottlecap $_LAYER_DIR/extensions/datadog-agent

  cd $_LAYER_DIR
    chmod 755 datadog_wrapper datadog-agent-go extensions/datadog-agent
    du -hs datadog_wrapper datadog-agent-go extensions/datadog-agent
    zip -r datadog_bottlecap-$ARCH.zip datadog_wrapper datadog-agent-go extensions/datadog-agent
    mv datadog_bottlecap-$ARCH.zip ..
  cd -

  du -hs $_LAYER_DIR/../datadog_bottlecap-$ARCH.zip
}

build_dockerized(){
  _require_argument ARCH
  # remove the target folder to avoid copying 12gb and invalidate cache during dockerized build
  rm -rf ../bottlecap/target
  cd ../scripts
    ARCHITECTURE=$ARCH ./build_bottlecap_layer.sh
  cd -
}

publish() {
  _require_argument ARCH
  _require_argument LAYER_NAME
  local _LAYER_PATH=../.layers/datadog_bottlecap-$ARCH.zip
  version_nbr=$(aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda publish-layer-version \
      --layer-name "${LAYER_NAME}" \
      --description "Datadog Bottlecap Beta" \
      --zip-file "fileb://${_LAYER_PATH}" \
      --region $_REGION | jq -r '.Version')
  echo "$version_nbr"
}

update() {
  _require_argument LAMBDA_NAME
  _require_argument LAYER_NAME
  aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda update-function-configuration \
    --function-name $LAMBDA_NAME \
    --layers "arn:aws:lambda:${_REGION}:425362996713:layer:${LAYER_NAME}:${1}" "arn:aws:lambda:us-east-1:425362996713:layer:Datadog-Node20-x:116" > /dev/null
  echo "DONE: Updated function ${LAMBDA_NAME} with layer ${LAYER_NAME} version ${1}"
}

restart() {
  _require_argument LAMBDA_NAME
  local LAMBDA_NAME=${1}
  local TIMEOUT=$((${2} + 30))
  aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda update-function-configuration \
    --function-name ${LAMBDA_NAME} \
    --timeout $TIMEOUT > /dev/null
}

invoke(){
  _require_argument LAMBDA_NAME
  aws-vault exec sso-serverless-sandbox-account-admin -- time aws lambda invoke \
    --function-name ${LAMBDA_NAME} \
    --payload '{"message": "Hello from Lambda!"}' \
    /dev/stdout
  echo "DONE: Invoked function ${LAMBDA_NAME}"
}

all(){
#  time build_local
  time build_dockerized
  version_nbr=$(publish)
  update "$version_nbr"
  invoke
}

format () {
  cd ../bottlecap
    cargo fmt --all
    cargo clippy --workspace --all-features
    cargo clippy --fix
  cd -
}

time "$@"
