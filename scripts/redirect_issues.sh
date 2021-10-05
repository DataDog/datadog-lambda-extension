#!/bin/bash
set -euo pipefail
IFS=$'\n\t'
# Retrieve all issues from a repo

yesterday=$(date --iso-8601=seconds --date="yesterday")

GH_REPO_OWNER="DataDog"
GH_REPO="datadog-agent"
GH_URL="https://api.github.com/repos/$GH_REPO_OWNER/$GH_REPO/issues?since=$yesterday"

SLACK_CHANNEL="#serverless"

KEYWORDS="lambda|serverless|extension|layer"

response=$(curl -H "Authorization: token $GH_TOKEN" -H "Accept: application/vnd.github.v3+json" -X GET "$GH_URL")
echo "$response" | jq -c '.[]' | while read -r issue; do 
  # Check if the retrieved issue is not a PR
  if [[ $(echo "${issue}" | jq '.pull_request') == "null"  && \
     ( ! -z $(echo "${issue}" | jq '.body' | grep -E "$KEYWORDS") || \
     ! -z $(echo "${issue}" | jq '.title' | grep -E "$KEYWORDS") ) \
    ]]; then
      issue_url=$(echo "${issue}" | jq '.html_url')
      curl -H "Content-type: application/json" -X POST "$SLACK_WEBHOOK" -d '{"channel": "'$SLACK_CHANNEL'", "text": ":sos: '$issue_url'"}'  
  fi
done
