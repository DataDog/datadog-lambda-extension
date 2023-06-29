#!/bin/bash

# This script is used to generate release notes for the DataDog/datadog-lambda-extension repository.
# The script fetches pull requests from the DataDog/datadog-agent repository that have been merged since the last release
# of DataDog/datadog-lambda-extension and are labeled with "team/serverless". 
# It generates a list of changes based on these pull requests.
#
# To invoke this script:
# ./get_all_extension_prs_since_last_release.sh
# The script will output the release notes directly in the terminal window.

# Get the date of the last release
last_release_date=$(curl -s "https://api.github.com/repos/DataDog/datadog-lambda-extension/releases/latest" | jq -r '.published_at')
last_release_date=$(date -u -d"$last_release_date" +"%s")

# get all PRs since the last release
page=1
echo "What's Changed"
while true
do
    response=$(curl -s "https://api.github.com/repos/DataDog/datadog-agent/pulls?state=closed&sort=updated&direction=desc&per_page=100&page=$page")
    prs_length=$(echo $response | jq -r '. | length')

    if [[ prs_length -eq 0 ]]; then
        break
    fi

    for (( i=0; i<$prs_length; i++ ))
    do
        merged_at=$(echo $response | jq -r ".[$i].merged_at")
        if [[ "$merged_at" == "null" ]]; then
            continue
        fi

        base_ref=$(echo $response | jq -r ".[$i].base.ref")
        if [[ "$base_ref" != "main" ]]; then
            continue
        fi

        merged_at=$(date -u -d"$merged_at" +"%s")
        if [[ $merged_at -lt $last_release_date ]]; then
            break 2
        fi

        labels=$(echo $response | jq -r ".[$i].labels[]?.name" | grep "team/serverless")
        if [[ "$labels" != "" ]]; then
            pr_number=$(echo $response | jq -r ".[$i].number")
            pr_title=$(echo $response | jq -r ".[$i].title")
            pr_sha=$(echo $response | jq -r ".[$i].merge_commit_sha" | cut -c1-7)
            echo "$pr_title DataDog/datadog-agent@$pr_sha"
        fi
    done

    ((page++))
done
