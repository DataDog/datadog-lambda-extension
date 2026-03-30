#!/bin/bash
# SMOKE TEST ONLY — do not merge to main
#
# This file exists solely to verify that the gitleaks Secrets Scan CI job
# correctly blocks PRs containing credential-shaped strings.
#
# After confirming the CI job fails on this branch, delete this file and
# remove this branch.
#
# See: .github/workflows/secrets-scan.yml
# See: PR #1134 test plan — "Optional smoke test" step

# Fake AWS access key that matches the AKIA[A-Z0-9]{16} pattern.
# This key is from official AWS documentation and is not a real credential.
# Source: https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_iam-quotas.html
FAKE_AWS_KEY="AKIAIOSFODNN7EXAMPLE"
FAKE_AWS_SECRET="wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"

echo "This file is intentionally flagged by gitleaks for smoke-test purposes."
echo "Key: $FAKE_AWS_KEY"
