#!/bin/bash
args=("$@")
if [ "$DD_EXPERIMENTAL_ENABLE_PROXY" == "true" ]
then
  if [ fgrep -ix "$DD_LOG_LEVEL" <<< "debug" ]
  then
    echo "[bootstrap] DD_EXPERIMENTAL_ENABLE_PROXY is true"
    echo "[bootstrap] original AWS_LAMBDA_RUNTIME_API value is $AWS_LAMBDA_RUNTIME_API"
  fi

  export AWS_LAMBDA_RUNTIME_API="127.0.0.1:9000"

  if [ fgrep -ix "$DD_LOG_LEVEL" <<< "debug" ]
  then
    echo "[bootstrap] rerouting AWS_LAMBDA_RUNTIME_API to $AWS_LAMBDA_RUNTIME_API"
  fi
fi
exec "${args[@]}"