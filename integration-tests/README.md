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
### Overview
* Gitlab runs test suites in parallel.
* For each test suite, Gitlab will:
  * Deploy the associated stack
  * Run the test suite
  * Destroy the associated stack
* You can download the test suite run from the Gitlab page. 

### Adding a New Test Suite

1. **Create test file**: `tests/<suite-name>.test.ts`
2. **Create CDK stacks**: `lib/stacks/<suite-name>.ts`
3. **Register stacks**: Add to `bin/app.ts`
4. **Update pipeline**: Add suite name to `.gitlab/datasources/test-suites.yaml`:


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


## Guidelines

### Naming
* An `identifier` is used to differentiate the different stacks. For local development, the identifier is automatically set using the command `whoami` and parses the user's first name. For gitlab pipelines, the identifier is the git commit short sha.
* Stacks should be named `integ-<identifier>-<stack name>`
* Lambda functions should be named `<stack-id>-<function name>`

### Stacks
* Stacks automatically get destroyed at the end of the gitlab integration tests step.
* Stacks should be setup to not retain resources. A helper function `createLogGroup` exists with `removalPolicy: cdk.RemovalPolicy.DESTROY`. 
* Stacks should include the tag `extension_integration_test: true`. This gets added in `app.ts`. This lets us easily run scripts to clean up old stacks in case the cleanup step was missed.
