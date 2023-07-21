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
last_release_date=$(date -u -d"$last_release_date" +"%Y-%m-%dT%H:%M:%SZ")

# Add one second to the last_release_date to avoid including PRs merged exactly at the release time
last_release_date=$(date -u -d"$last_release_date + 1 second" +"%Y-%m-%dT%H:%M:%SZ")

# Initialize page number
page=1

echo "What's Changed"

while true; do
    # Use GitHub's Search API to find all merged PRs in the DataDog/datadog-agent repo labeled "team/serverless" that were merged after the last release
    response=$(curl -s "https://api.github.com/search/issues?q=repo:DataDog/datadog-agent+label:team/serverless+merged:>=$last_release_date&type=pr&sort=updated&order=desc&per_page=100&page=$page")

    # Check if the response contains items, if not break the loop
    prs_length=$(echo $response | jq -r '.items | length')
    if [[ $prs_length -eq 0 ]]; then
        break
    fi

    # Iterate over all PRs and print their title and merge commit SHA
    for (( i=0; i<$prs_length; i++ )); do
        pr_title=$(echo $response | jq -r ".items[$i].title")
        pr_sha=$(echo $response | jq -r ".items[$i].pull_request.url" | xargs curl -s | jq -r ".merge_commit_sha")
        echo -e "+ $pr_title https://github.com/DataDog/datadog-agent/commit/$pr_sha\n"
    done

    ((page++))
done

echo "#############################################################"
echo "#                                                           #"
echo "#   IMPORTANT: Please verify these commits before integrating   #"
echo "#   them into the lambda extension release. (i.e. not every commit outputted #"
echo "#   here needs to be included in release) This script is primarily for     #"
echo "#   convenience and does not replace a thorough review process.  #"
echo "#                                                           #"
echo "#############################################################"
echo
echo "Also, remember to verify the list of commits at https://github.com/DataDog/datadog-agent/pulls?q=is%3Apr+label%3Ateam%2Fserverless+is%3Aclosed+sort%3Aupdated-desc"
echo "Please make an announcement in the serverless-backroom mentioning everyone whose commits are going to be pulled in. If their commit wasn't released, they should not close their JIRA ticket until they can assert it was released."
echo
