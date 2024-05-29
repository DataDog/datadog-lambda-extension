#!/bin/bash
set -e

FUNCTION_NAME="svls4825m-bottlecap-busy-node20-lambda"

source deploy_steps.sh

update() {
  aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda update-function-configuration \
    --function-name $FUNCTION_NAME \
    --layers "arn:aws:lambda:${REGION}:425362996713:layer:${LAYER_NAME}:${1}" "arn:aws:lambda:us-east-1:425362996713:layer:Datadog-Node20-x:116"
  echo "DONE: Updated function ${FUNCTION_NAME} with layer ${LAYER_NAME} version ${1}"
}

force_update() {
  local F_NAME=${1}
  local TIMEOUT=$((${2} + 30))
  aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda update-function-configuration \
    --function-name ${F_NAME} \
    --timeout $TIMEOUT > /dev/null
}

invoke(){
  local F_NAME=${1}
  aws-vault exec sso-serverless-sandbox-account-admin -- time aws lambda invoke \
    --function-name ${F_NAME} \
    --payload '{"message": "Hello from Lambda!"}' \
    /dev/stdout
  echo "DONE: Invoked function ${F_NAME}"
}

all(){
  time zip_extension
  version_nbr=$(publish)
  update "$version_nbr"
  invoke $FUNCTION_NAME
}

cold_call() {
    for i in {1..10}; do
      force_update $FUNCTION_NAME $i && invoke $FUNCTION_NAME
    done
}

call() {
  invoke $FUNCTION_NAME
}

"$@"