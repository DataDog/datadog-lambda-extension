#!/bin/bash
args=("$@")

# lowercase environment variables
DD_LOG_LEVEL=$(echo "$DD_LOG_LEVEL" | tr '[:upper:]' '[:lower:]')
DD_EXPERIMENTAL_ENABLE_PROXY=$(echo "$DD_EXPERIMENTAL_ENABLE_PROXY" | tr '[:upper:]' '[:lower:]')
DD_OPTIMIZE_COLD_START=$(echo "$DD_OPTIMIZE_COLD_START" | tr '[:upper:]' '[:lower:]')

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

# Optimize cold start performance for Java functions
if [ "$DD_OPTIMIZE_COLD_START" != "false" ]
then
  # Stopping tiered compilation at level 1 reduces the time the JVM spends optimizing code
  export _JAVA_OPTIONS="-XX:+TieredCompilation -XX:TieredStopAtLevel=1"
fi

exec "${args[@]}"