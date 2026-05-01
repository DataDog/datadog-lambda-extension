#!/bin/bash
curl -OL "binaries.ddbuild.io/dd-source/authanywhere/LATEST/authanywhere-linux-amd64" \
    && mv "authanywhere-linux-amd64" /bin/authanywhere \
    && chmod +x /bin/authanywhere

BTI_CI_API_TOKEN=$(authanywhere --audience rapid-devex-ci)

BTI_RESPONSE=$(curl --silent --request GET \
    --header "$BTI_CI_API_TOKEN" \
    --header "Content-Type: application/vnd.api+json" \
    "https://bti-ci-api.us1.ddbuild.io/internal/ci/gitlab/token?owner=DataDog&repository=datadog-lambda-extension")

GITLAB_TOKEN=$(echo "$BTI_RESPONSE" | jq -r '.token // empty')

if [ -z "$GITLAB_TOKEN" ]; then
    echo "ERROR: Failed to obtain GitLab token. BTI HTTP status: $(echo "$BTI_RESPONSE" | jq -r '.status // "unknown"')"
    exit 1
fi

URL="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/pipelines/${CI_PIPELINE_ID}/bridges"

echo "Fetching E2E job status from: $URL"

while true; do
    HTTP_STATUS=$(curl -s -o /tmp/e2e_response.json -w "%{http_code}" --header "PRIVATE-TOKEN: ${GITLAB_TOKEN}" "$URL")
    if [ "$HTTP_STATUS" != "200" ]; then
        echo "WARNING: GitLab API returned HTTP $HTTP_STATUS, retrying in 2 minutes..."
        sleep 120
        continue
    fi
    RESPONSE=$(cat /tmp/e2e_response.json)
    E2E_JOB_STATUS=$(echo "$RESPONSE" | jq -r --arg name "$E2E_JOB_NAME" '.[] | select(.name==$name) | .downstream_pipeline.status')
    echo -n "E2E job status: $E2E_JOB_STATUS, "
    if [ "$E2E_JOB_STATUS" == "success" ]; then
        echo "E2E tests completed successfully"
        exit 0
    elif [ "$E2E_JOB_STATUS" == "failed" ]; then
        echo "E2E tests failed"
        exit 1
    elif [ "$E2E_JOB_STATUS" == "running" ]; then
        echo "E2E tests are still running, retrying in 2 minutes..."
    elif [ "$E2E_JOB_STATUS" == "canceled" ]; then
        echo "E2E tests were canceled"
        exit 1
    elif [ "$E2E_JOB_STATUS" == "skipped" ]; then
        echo "E2E tests were skipped"
        exit 0
    else
        echo "Unknown E2E test status: $E2E_JOB_STATUS, retrying in 2 minutes..."
    fi
    sleep 120
done
