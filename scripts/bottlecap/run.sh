#!/bin/bash
set -e

FUNCTION_NAME="agbtc2-bottlecap-busy-node20-lambda"

source publish_sandbox.sh
source build.sh

update() {
  aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda update-function-configuration \
    --function-name $FUNCTION_NAME \
    --layers "arn:aws:lambda:${REGION}:425362996713:layer:${LAYER_NAME}:${1}"
  echo "DONE: Updated function ${FUNCTION_NAME} with layer ${LAYER_NAME} version ${1}"
}

invoke(){
  aws-vault exec sso-serverless-sandbox-account-admin -- aws lambda invoke \
    --function-name ${FUNCTION_NAME} \
    --payload '{"message": "Hello from Lambda!"}' \
    /dev/stdout
  echo "DONE: Invoked function ${FUNCTION_NAME}"
}

all(){
  time zip_extension
  version_nbr=$(publish)
  update "$version_nbr"
  invoke
}

call() {
  invoke
}

"$@"