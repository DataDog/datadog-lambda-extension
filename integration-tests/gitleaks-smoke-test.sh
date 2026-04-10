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
# NOT a real credential — generated for smoke-test purposes only.
# Deliberately avoids the canonical AWS doc examples (AKIAIOSFODNN7EXAMPLE)
# which are in gitleaks' internal global allowlist and would not be flagged.
FAKE_AWS_KEY="AKIAT3STFAKEKEY12345"
FAKE_AWS_SECRET="sM0keT3st+FaKeK3y/ABCDEFGHIJ1234567890ab"

echo "This file is intentionally flagged by gitleaks for smoke-test purposes."
echo "Key: $FAKE_AWS_KEY"
