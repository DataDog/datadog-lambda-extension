# Datadog Lambda Extension Integration Tests

This directory contains integration tests for the Datadog Lambda Extension. 

The general flow is:
1. Deploy test setup using CDK.
2. Invoke lambda function.
3. Call Datadog to get telemetry data. 
4. Check telemtry data based on test requirements.

For simplicity, integraiton tests are setup to only test against ARM runtimes.

### Local Development

1. **Datadog API Keys**: Set environment variables
   ```bash
   export DD_API_KEY="your-datadog-api-key"
   export DD_APP_KEY="your-datadog-app-key"
   ```

2. **AWS Secrets Manager**: You need a secret ARN for the Datadog API key that Lambda functions will use
   ```bash
   export DATADOG_API_SECRET_ARN="arn:aws:secretsmanager:us-east-1:ACCOUNT_ID:secret:YOUR_SECRET"
   ```

## Local Testing Workflow

### 1. Build and Deploy Extension Layer

First, publish your extension layer with a unique suffix:

```bash
cd /path/to/datadog-lambda-extension

# Example: scripts/publish_bottlecap_sandbox.sh <suffix>
TODO
```

### 2. Deploy Test Stacks

Deploy the CDK stacks that create Lambda functions for testing:

```bash
cd integration-tests
./scripts/deploy.sh ExampleTestStack yourname
```

### 3. Run Integration Tests

Run Jest tests that invoke Lambda functions and verify Datadog telemetry:

```bash
export SUFFIX="yourname" 
export DD_API_KEY="your-datadog-api-key"
export DD_APP_KEY="your-datadog-app-key"

# Run tests
npm test
```

**Note**: Tests wait 10 minutes after Lambda invocation to allow telemetry to appear in Datadog. The total test timeout is ~11.6 minutes per test.


## GitLab CI/CD Pipeline

The integration tests run automatically on merge requests:

### Pipeline Stages

1. **`publish integration layer (arm64)`**
   - Publishes an arm64 extension layer with suffix: `${CI_COMMIT_SHORT_SHA}`
   - Layer name: `Datadog-Extension-<commit-hash>`
   - Saves layer ARN as artifact

2. **`integration-deploy`**
   - Deploys all CDK stacks with suffix: `${CI_COMMIT_SHORT_SHA}`
   - Uses the layer ARN from previous step
   - Sets `SUFFIX=${CI_COMMIT_SHORT_SHA}`

3. **`integration-test`**
   - Runs Jest test suite
   - Uses `SUFFIX=${CI_COMMIT_SHORT_SHA}` to find deployed functions
   - DD_API_KEY and DD_APP_KEY retrieved from AWS SSM Parameter Store
   - Generates JUnit XML and HTML reports

4. **`integration-cleanup-stacks`**
   - Destroys all stacks with the commit hash suffix
   - Runs even if tests fail (`when: always`)

5. **`integration-cleanup-layers`**
   - Deletes all layer versions with the commit hash suffix
   - Runs even if tests fail (`when: always`)


