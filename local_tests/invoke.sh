#!/bin/bash

NB_INVOKE=$1

# Start the container
dockerId=$(docker run -d -p 9000:8080 datadog/extension-local-tests)

# Curl it!
i=0
while true; do 
    i=$((i+1))
    echo "Invoke # $i"
    curl -XPOST "http://localhost:9000/2015-03-31/functions/function/invocations" -d '{}' ; echo
    if [[ $i -gt $NB_INVOKE ]]
    then
        echo "Saving logs to logs.txt"
        docker logs $dockerId >./local_tests/logs.txt 2>&1
        echo "Stoping"
        docker stop $dockerId
        exit 0
    fi
    sleep 1
done