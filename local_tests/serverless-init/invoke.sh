#!/bin/bash

N_INVOKE=$1

# Start the container
dockerId=$(docker run -d -p 9000:8080 -p 127.0.0.1:8124:8124 datadog/extension-local-tests)

i=0
while true; do
  i=$((i + 1))
  echo "Invoke # $i"
  curl -XGET "http://localhost:9000"
  echo
  sleep 1
  if [[ $i -ge $N_INVOKE ]]; then
    echo "Saving logs to logs.txt"
    docker logs $dockerId >logs.txt 2>&1
    echo "Stopping"
    docker stop $dockerId
    exit 0
  fi
  sleep 1
done
