# Datadog Lambda Extension Integration Tests

This directory contains integration tests for the Datadog Lambda Extension. 

Each test suite has a cdk stack and an associated test file. Example, `base.ts` and `base.tests.ts`. Test suits are run in parallel in Gitlab CI/CD pipeline.

The general flow is:
1. Deploy test setup using CDK.
2. Invoke lambda functions.
3. Wait for data to propagate to Datadog.
4. Call Datadog to get telemetry data and check the data based on test requirements.

For simplicity, integration tests are set up to only test against ARM runtimes and 4 runtimes (Python, Node, Java, and Dotnet).


## Test Suites

### Base Tests

The base test suite provides basic functionality tests across all supported Lambda runtimes. These tests verify core extension functionality without additional instrumentation.

**What it tests:**
- Extension can collect and forward logs to Datadog
- Extension generates and sends traces with proper span structure
- Extension detects cold starts correctly

**Test Coverage:**
- Lambda invocation succeeds (200 status code)
- "Hello world!" log message is sent to Datadog
- One trace is sent to Datadog
- `aws.lambda` span exists with correct properties including `cold_start: 'true'`
- `aws.lambda.cold_start` span is created
- `aws.lambda.load` span is created for Python and Node

### OTLP Tests

The OTLP test suite verifies OpenTelemetry Protocol (OTLP) integration with the Datadog Lambda Extension. These tests use Lambda functions instrumented with OpenTelemetry SDKs to ensure telemetry data flows correctly through the extension to Datadog.

**What it tests:**
- Lambda functions instrumented with OpenTelemetry SDKs can invoke successfully
- Traces are properly sent to Datadog via OTLP
- Spans contain correct structure and attributes

**Test Coverage:**
- Lambda invocation succeeds (200 status code)
- At least one trace is sent to Datadog
- Trace contains valid spans with proper structure

## CI/CD Pipeline Structure


### Pipeline Flow

```
                                    ┌→ deploy-base → test-base → cleanup-base ┐
publish layer → build lambdas ─────┤                                          ├→ cleanup-layer
                                    └→ deploy-otlp → test-otlp → cleanup-otlp ┘
```

### Test Suite Lifecycle

Each test suite (base, otlp, etc.) follows this lifecycle:

1. **Deploy**: Deploys only the stacks for that suite
   - Pattern: `cdk deploy "integ-${IDENTIFIER}-${TEST_SUITE}"`
   - Example: `integ-abc123-base` deploys all base test stacks

2. **Test**: Runs only the tests for that suite
   - Command: `jest tests/${TEST_SUITE}.test.ts`
   - Example: `jest tests/base.test.ts`

3. **Cleanup**: Removes only the stacks for that suite
   - Runs with `when: always` to ensure cleanup on failure
   - Pattern: Deletes all stacks matching `integ-${IDENTIFIER}-${TEST_SUITE}`

### Adding a New Test Suite

To add a new test suite to the parallel execution:

1. **Create test file**: `tests/<suite-name>.test.ts`
2. **Create CDK stacks**: `lib/stacks/<suite-name>.ts`
3. **Register stacks**: Add to `bin/app.ts`
4. **Update pipeline**: Add suite name to `.gitlab/templates/pipeline.yaml.tpl`:
   ```yaml
   parallel:
     matrix:
       - TEST_SUITE: [base, otlp, <suite-name>]
   ```

## Guidelines

### Naming
* An `identifier` is used to differentiate the different stacks. For local development, the identifier is automatically set using the command `whoami` and parses the user's first name. For gitlab pipelines, the identifier is the git commit short sha.
* Stacks should be named `integ-<identifier>-<stack name>`
* Lambda functions should be named `<stack-id>-<function name>`

### Stacks
* Stacks automatically get destroyed at the end of the gitlab integration tests step. Stack should be setup to not retain resources. A helper function `createLogGroup` exists with `removalPolicy: cdk.RemovalPolicy.DESTROY`. 


## Local Development

### Prerequisites

**Datadog API Keys**: Set environment variables
```bash
export DD_API_KEY="your-datadog-api-key"
export DD_APP_KEY="your-datadog-app-key"
export DATADOG_API_SECRET_ARN="arn:aws:secretsmanager:us-east-1:ACCOUNT_ID:secret:YOUR_SECRET"
```

**Docker**: Required for building Lambda functions.

### Workflow

#### 1. Build and Publish Extension Layer

Publish your extension layer to AWS Lambda:

```bash
./scripts/local_publish.sh
```

This creates and publishes `Datadog-Extension-ARM-<your-name>` with the latest version number.

#### 2. Deploy Test Stacks

Deploy CDK stacks that create Lambda functions for testing:

```bash
./scripts/local_deploy.sh <stack-name>
```

This creates `integ-<your-name>-<stack-name>` and automatically:
- Builds required Lambda functions based on the stack name
- Uses the extension layer created in step 1
- Deploys the stack to AWS

**Examples:**
```bash
# Deploy base test stack
./scripts/local_deploy.sh base

```

#### 3. Run Integration Tests

Run Jest tests that invoke Lambda functions and verify Datadog telemetry:

```bash
# Run all tests
npm test

# Run specific test file
npm test -- base.test.ts

```

**Note**: Tests wait several minutes after Lambda invocation to allow telemetry to propagate to Datadog.
