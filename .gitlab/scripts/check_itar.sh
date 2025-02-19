#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2025 Datadog, Inc.

set -e

if [ -z "$GITLAB_USER_EMAIL" ]; then
    printf "[Error] No GITLAB_USER_EMAIL found.\n"
    printf "Exiting script...\n"
    exit 1
fi


ITAR_APPROVED_USERS=(
    "aleksandr.pasechnik@datadoghq.com"
    "other@datadoghq.com"
)

if [[ " ${ITAR_APPROVED_USERS[@]} " =~ " ${GITLAB_USER_EMAIL} " ]]; then
    echo "Access granted for $GITLAB_USER_EMAIL"
    exit 0
else
    echo "Access denied for $GITLAB_USER_EMAIL"
    exit 1
fi
