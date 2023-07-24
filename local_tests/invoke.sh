#!/bin/bash

NB_INVOKE=$1

# Start the container
dockerId=$(docker run -d -p 9000:8080 -p 8124:8124 datadog/extension-local-tests)

# Curl it!
i=0
while true; do
    i=$((i+1))
    echo "Invoke # $i"
    if [ "${RUNTIME}" == "java" ]; then
        echo "java runtime detected"
        curl -XPOST "http://localhost:9000/2015-03-31/functions/function/invocations" -H "Content-Type: application/json" -d @"./local_tests/java/api-gateway.json"&
        sleep 1
        curl -XPOST "http://localhost:8124/lambda/flush" #trigger manual flush for universal-instrumentation enabled runtimes
    else
        curl -XPOST "http://localhost:9000/2015-03-31/functions/function/invocations" -d '{}' ; echo
    fi
    sleep 1
    if [[ $i -ge $NB_INVOKE ]]
    then
        echo "Saving logs to logs.txt"
        docker logs $dockerId >./local_tests/logs.txt 2>&1
        echo "Stopping"
        docker stop $dockerId
        exit 0
    fi
    sleep 1
done