#!/bin/sh
if [ -z "${AWS_LAMBDA_RUNTIME_API}" ]; then
  echo "Running in RIE mode"
  exec /usr/local/bin/aws-lambda-rie /var/runtime/bootstrap index.handler
else
  echo "Running in non-RIE mode"
  exec /var/runtime/bootstrap index.handler
fi
