#!/bin/bash
args=("$@")
if [ "$DD_EXPERIMENTAL_ENABLE_PROXY" == "true" ]
then
  echo "[bootstrap] DD_EXPERIMENTAL_ENABLE_PROXY is true"
  echo "[bootstrap] orignal AWS_LAMBDA_RUNTIME_API $AWS_LAMBDA_RUNTIME_API"
  export AWS_LAMBDA_RUNTIME_API="127.0.0.1:9000"
  echo "[bootstrap] rerouting to $AWS_LAMBDA_RUNTIME_API"
fi
exec "${args[@]}"