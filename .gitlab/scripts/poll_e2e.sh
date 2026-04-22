#!/usr/bin/env bash
# Trigger serverless-e2e-tests pipeline and poll until completion.

set -euo pipefail

if [ -z "${EXTENSION_LAYER_ARN:-}" ]; then
    echo "ERROR: EXTENSION_LAYER_ARN is not set or empty"
    exit 1
fi

E2E_PROJECT_ENCODED="DataDog%2Fserverless-e2e-tests"
E2E_REF="${E2E_REF:-main}"

curl -OL "binaries.ddbuild.io/dd-source/authanywhere/LATEST/authanywhere-linux-amd64" && mv authanywhere-linux-amd64 /bin/authanywhere && chmod +x /bin/authanywhere

BTI_CI_API_TOKEN=$(authanywhere --audience rapid-devex-ci)

BTI_RESPONSE=$(curl --silent --request GET \
    --header "$BTI_CI_API_TOKEN" \
    --header "Content-Type: application/vnd.api+json" \
    "https://bti-ci-api.us1.ddbuild.io/internal/ci/gitlab/token?owner=DataDog&repository=serverless-e2e-tests")

GITLAB_TOKEN=$(echo "$BTI_RESPONSE" | jq -r '.token // empty')
if [ -z "$GITLAB_TOKEN" ]; then
    echo "ERROR: could not obtain GitLab token from BTI"
    exit 1
fi

echo "Triggering DataDog/serverless-e2e-tests pipeline (ref: ${E2E_REF})..."
echo "  EXTENSION_LAYER_ARN=${EXTENSION_LAYER_ARN}"

TRIGGER_RESPONSE=$(curl --silent --request POST \
    --header "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
    --header "Content-Type: application/json" \
    --data "$(jq -n \
        --arg ref "$E2E_REF" \
        --arg arn "$EXTENSION_LAYER_ARN" \
        '{ref: $ref, variables: [{key: "EXTENSION_VERSION", value: $arn}]}')" \
    "${CI_API_V4_URL}/projects/${E2E_PROJECT_ENCODED}/pipeline")

PIPELINE_ID=$(echo "$TRIGGER_RESPONSE" | jq -r '.id // empty')
PIPELINE_URL=$(echo "$TRIGGER_RESPONSE" | jq -r '.web_url // empty')

if [ -z "$PIPELINE_ID" ] || [ "$PIPELINE_ID" = "null" ]; then
    echo "ERROR: failed to trigger downstream pipeline"
    echo "Response: $TRIGGER_RESPONSE"
    exit 1
fi

echo "Triggered downstream pipeline: ${PIPELINE_URL} (ID: ${PIPELINE_ID})"

while true; do
    STATUS=$(curl --silent \
        --header "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
        "${CI_API_V4_URL}/projects/${E2E_PROJECT_ENCODED}/pipelines/${PIPELINE_ID}" \
        | jq -r '.status // empty')

    echo -n "E2E pipeline ${PIPELINE_ID} status: ${STATUS}, "

    case "$STATUS" in
        success)
            echo "E2E tests passed"
            exit 0
            ;;
        failed)
            echo "E2E tests failed"
            exit 1
            ;;
        canceled|canceling)
            echo "E2E tests canceled"
            exit 1
            ;;
        skipped)
            echo "E2E tests skipped"
            exit 0
            ;;
        running|pending|created|waiting_for_resource|preparing)
            echo "still running, checking again in 2 minutes..."
            ;;
        *)
            echo "unknown status, checking again in 2 minutes..."
            ;;
    esac

    sleep 120
done
