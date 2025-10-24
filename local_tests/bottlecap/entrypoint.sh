#!/bin/sh
if [ -z "${AWS_LAMBDA_RUNTIME_API}" ]; then
  echo "Running in RIE mode"
  exec /usr/local/bin/aws-lambda-rie /usr/bin/npx aws-lambda-ric index.handler
else
  echo "Running in non-RIE mode"
  exec /usr/bin/npx aws-lambda-ric index.handler
fi
