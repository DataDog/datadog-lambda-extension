# Datadog Lambda Extension Integration Tests

This directory contains CDK-based integration tests for the Datadog Lambda Extension. Tests deploy Lambda functions with the extension, invoke them, and verify that telemetry data (logs, traces, metrics) is in Datadog.

## Architecture

- **CDK Stacks**: Define Lambda functions with the Datadog extension layer
- **Jest Tests**: Invoke Lambda functions and verify data in Datadog
- **GitLab CI/CD**: Automated testing on merge requests

## Prerequisites

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
# From the root of datadog-lambda-extension repository
cd /path/to/datadog-lambda-extension

# Build and publish the layer
# Example: scripts/publish_bottlecap_sandbox.sh <suffix>
# This will create a layer named: Datadog-Extension-<suffix>
```

### 2. Deploy Test Stacks

Deploy the CDK stacks that create Lambda functions for testing:

```bash
cd integration-tests

# Option 1: Set explicit suffix (recommended)
export SUFFIX="yourname"
./scripts/deploy.sh ExampleTestStack yourname

# Option 2: Let it auto-detect from username (john.doe -> john)
./scripts/deploy.sh ExampleTestStack yourname

# Or deploy manually:
# Get the extension layer ARN
export EXTENSION_LAYER_ARN=$(aws-vault exec sso-serverless-sandbox-account-admin -- \
  aws lambda list-layer-versions \
  --layer-name Datadog-Extension-yourname \
  --query 'LayerVersions[0].LayerVersionArn' \
  --output text)

# Set the Datadog secret ARN
export DATADOG_API_SECRET_ARN="arn:aws:secretsmanager:us-east-1:ACCOUNT_ID:secret:YOUR_SECRET"

# Build and deploy
npm run build
aws-vault exec sso-serverless-sandbox-account-admin -- \
  cdk deploy ExampleTestStack-yourname --require-approval never
```

### 3. Run Integration Tests

Run Jest tests that invoke Lambda functions and verify Datadog telemetry:

```bash
# Set required environment variables
export SUFFIX="yourname"  # Optional: will auto-detect from username if not set
export DD_API_KEY="your-datadog-api-key"
export DD_APP_KEY="your-datadog-app-key"

# Run tests
npm test

# Or run with watch mode
npm run test:watch

# Or run with coverage
npm run test:coverage
```

**Note**: Tests wait 10 minutes after Lambda invocation to allow telemetry to appear in Datadog. The total test timeout is ~11.6 minutes per test.

### 4. Destroy Test Stacks

Clean up resources after testing:

```bash
# Set your suffix
export SUFFIX="yourname"

# Destroy all stacks with your suffix
aws-vault exec sso-serverless-sandbox-account-admin -- \
  cdk destroy "*-yourname" --force
```

## Available Stacks

### ExampleTestStack

Simple "Hello World" Lambda function that:
- Uses Node.js 20 runtime
- Has the Datadog extension layer attached
- Logs a message and returns a response
- Function name: `exampleTestFunction-<suffix>`

### OtlpTraceTestStack

Lambda function for testing OTLP trace ingestion:
- Tests OpenTelemetry trace format support
- Function name: TBD based on stack definition

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

### Pipeline Environment Variables

The pipeline automatically sets:
- `SUFFIX`: Set to `${CI_COMMIT_SHORT_SHA}` (e.g., `abc12345`)
- `EXTENSION_LAYER_ARN`: Retrieved from published layer artifact
- `DD_API_KEY`: Retrieved from AWS SSM Parameter Store
- `DD_APP_KEY`: Retrieved from AWS SSM Parameter Store
- `DATADOG_API_SECRET_ARN`: Should be set for Lambda functions (TBD in environments config)

## Configuration

### Environment Variables

| Variable | Description | Required | Local | CI/CD |
|----------|-------------|----------|-------|-------|
| `SUFFIX` | Unique identifier for stacks/functions/layers | Yes | Set manually | Auto (`CI_COMMIT_SHORT_SHA`) |
| `EXTENSION_LAYER_ARN` | ARN of the Datadog extension layer | Yes | From AWS CLI | From artifact |
| `DATADOG_API_SECRET_ARN` | Secret ARN for Lambda to access DD API key | Yes | Set manually | TBD |
| `DD_API_KEY` | Datadog API key for test queries | Yes | Set manually | From SSM |
| `DD_APP_KEY` | Datadog APP key for test queries | Yes | Set manually | From SSM |

### CDK Configuration

CDK apps read configuration from environment variables:
- `process.env.SUFFIX`: Used to name stacks and resources
- `process.env.EXTENSION_LAYER_ARN`: Layer to attach to Lambda functions
- `process.env.DATADOG_API_SECRET_ARN`: Secret for Lambda functions

**Suffix Resolution**:
1. `process.env.SUFFIX` (if set)
2. First name from `os.userInfo().username` (e.g., `john` from `john.doe`)
3. Default: `local-testing`

## Test Structure

### Test Files

- `tests/example.test.ts`: Tests for example Lambda function
- `tests/utils/invoke.ts`: Helper to invoke Lambda and query Datadog
- `tests/utils/lambda.ts`: Lambda invocation utilities
- `tests/utils/datadog.ts`: Datadog API query utilities
- `tests/utils/config.ts`: Configuration and naming utilities

### Test Flow

1. **Invoke Lambda**: Call the function with cold start
2. **Wait for telemetry**: 10 minute delay for data to appear in Datadog
3. **Query Datadog**: Fetch logs and traces by service name and request ID
4. **Verify data**: Assert expected logs/traces are present

## Troubleshooting

### Tests timing out or failing to find data

- Verify the Lambda function was invoked successfully (check CloudWatch Logs)
- Check Datadog for the service name: `exampleTestFunction-<suffix>`
- Verify DD_API_KEY and DD_APP_KEY are valid
- Ensure the extension layer is properly attached to the Lambda function
- Check the extension logs for errors (filter CloudWatch Logs by `DD_EXTENSION`)

### Stack deployment fails

- Verify EXTENSION_LAYER_ARN points to a valid layer
- Check DATADOG_API_SECRET_ARN is a valid secret
- Ensure AWS credentials have necessary permissions
- Check for existing stacks with the same suffix

### Layer not found

- Verify the layer was published with the correct suffix
- Check the layer exists in the correct region (us-east-1)
- Ensure you're using the correct AWS account

## Development

### Adding New Test Stacks

1. Create a new stack in `lib/` (e.g., `my-test-stack.ts`)
2. Import and instantiate in `bin/app.ts`
3. Create corresponding Lambda code in `lambda/`
4. Write tests in `tests/`

### Adding New Tests

1. Create a test file in `tests/`
2. Use `getFunctionName()` helper to build function names with suffix
3. Use `invokeLambdaAndGetDatadogData()` to invoke and get telemetry
4. Assert expected behavior

## References

- [AWS CDK Documentation](https://docs.aws.amazon.com/cdk/)
- [Jest Documentation](https://jestjs.io/)
- [Datadog API Documentation](https://docs.datadoghq.com/api/)

