#!/bin/bash
args=("$@")

# lowercase DD_LOG_LEVEL
DD_LOG_LEVEL=$(echo "$DD_LOG_LEVEL" | tr '[:upper:]' '[:lower:]')

if [ "$DD_EXPERIMENTAL_ENABLE_PROXY" == "true" ]
then
  if [ "$DD_LOG_LEVEL" == "debug" ]
  then
    echo "[bootstrap] DD_EXPERIMENTAL_ENABLE_PROXY is true"
    echo "[bootstrap] original AWS_LAMBDA_RUNTIME_API value is $AWS_LAMBDA_RUNTIME_API"
  fi

  export AWS_LAMBDA_RUNTIME_API="127.0.0.1:9000"

  if [ "$DD_LOG_LEVEL" == "debug" ]
  then
    echo "[bootstrap] rerouting AWS_LAMBDA_RUNTIME_API to $AWS_LAMBDA_RUNTIME_API"
  fi
fi
exec "${args[@]}"