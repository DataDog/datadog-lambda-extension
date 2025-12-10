# Datadog Lambda Extension Integration Tests

This directory contains integration tests for the Datadog Lambda Extension. 

The general flow is:
1. Deploy test setup using CDK.
2. Invoke lambda functions.
3. Wait for data to propagate to Datadog.
4. Call Datadog to get telemetry data and check the data based on test requirements.

For simplicity, integration tests are setup to only test against ARM runtimes.

## Test Suites

### Base Tests

The base test suite provides basic functionality tests across all supported Lambda runtimes. Also serves as an example for other tests.

The base tests verify the extension can:
- Collect and forward logs to Datadog
- Generate and send traces with proper span structure
- Detect cold starts

**Test Coverage:**
- Lambda invocation succeeds (200 status code)
- "Hello world!" log message is sent to Datadog
- One trace is sent to Datadog
- `aws.lambda` span exists with correct properties including `cold_start: 'true'`
- `aws.lambda.cold_start` span is created
- `aws.lambda.load` spand is created for python and node.

**Build Requirements:**

For Java and .NET tests, Lambda functions must be built before deployment:

```bash
# Build Java Lambda (uses Docker)
cd lambda/base-java && ./build.sh

# Build .NET Lambda (uses Docker)
cd lambda/base-dotnet && ./build.sh
```

These builds use Docker to ensure cross-platform compatibility and do not require local Maven or .NET SDK installation.

## Guidelines

### Naming
* An `identifier` is used to differentiate the different stacks. For local development, the identifier is automatically set using the command `whoami` and parses the user's first name. For gitlab pipelines, the identifier is the git commit short sha.
* Stacks should be named `integ-<identifier>-<stack name>`
* Lambda functions should be named `<stack-id>-<function name>`

### Stacks
* Stacks automatically get destroyed at the end of the gitlab integration tests step. Stack should be setup to not retain resources. A helper function `createLogGroup` exists with `removalPolicy: cdk.RemovalPolicy.DESTROY`. 


## Local Development

### Prerequisites Set env variables
**Datadog API Keys**: Set environment variables
   ```bash
   export DD_API_KEY="your-datadog-api-key"
   export DD_APP_KEY="your-datadog-app-key"
   export DATADOG_API_SECRET_ARN="arn:aws:secretsmanager:us-east-1:ACCOUNT_ID:secret:YOUR_SECRET"
   ```

### 1. Build and Deploy Extension Layer

First, publish your extension layer. 

```bash
./scripts/local_publish.sh
```

This will create and publish `Datadog-Extension-ARM-<your name>`.

### 2. Deploy Test Stacks

Deploy the CDK stacks that create Lambda functions for testing.

```bash
./scripts/local_deploy.sh <stack name> 
```

This will create `integ-<your name>-<stack name>`. The stacks will use the lambda extension created in the previous step.

### 3. Run Integration Tests

Run Jest tests that invoke Lambda functions and verify Datadog telemetry:

```bash
# All tests
npm test

# Single test
npm test -- <my test file>
```



**Note**: Tests wait for a few minutes after Lambda invocation to allow telemetry to appear in Datadog. 


