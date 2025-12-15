# Datadog Lambda Extension Integration Tests

This directory contains integration tests for the Datadog Lambda Extension. 

The general flow is:
1. Deploy test setup using CDK.
2. Invoke lambda functions.
3. Wait for data to propagate to Datadog.
4. Call Datadog to get telemetry data and check the data based on test requirements.

For simplicity, integration tests are setup to only test against ARM runtimes.

## Supported Runtimes

All test suites cover the following Lambda runtimes:
- **Java 21** - Eclipse Temurin
- **.NET 8** - Alpine
- **Python 3.12/3.13** - Standard Python runtime
- **Node.js 20** - Alpine

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

## Building Lambda Functions

Lambda functions with compiled languages (Java, .NET) or external dependencies (Python, Node.js with packages) must be built before deployment. Build scripts use Docker for cross-platform compatibility and don't require local toolchains.

### Build Scripts

Runtime-specific build scripts are located in `scripts/`:

```bash
# Build all Java Lambda functions
./scripts/build-java.sh

# Build all .NET Lambda functions
./scripts/build-dotnet.sh

# Build all Python Lambda functions
./scripts/build-python.sh

# Build all Node.js Lambda functions
./scripts/build-node.sh
```

You can also build a specific function by providing its path:

```bash
# Build a specific Java function
./scripts/build-java.sh lambda/otlp-java

# Build a specific .NET function
./scripts/build-dotnet.sh lambda/base-dotnet
```

**Note:** The `local_deploy.sh` script automatically builds required Lambda functions based on the stack name, so manual building is optional for local development.

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

**Docker**: Required for building Lambda functions. [Install Docker](https://docs.docker.com/get-docker/)

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
# Deploy base Java test
./scripts/local_deploy.sh base-java-stack

# Deploy OTLP Python test
./scripts/local_deploy.sh otlp-python-stack
```

**Available Stacks:**
- `base-java-stack`, `otlp-java-stack`
- `base-dotnet-stack`, `otlp-dotnet-stack`
- `base-python-stack`, `otlp-python-stack`
- `base-node-stack`, `otlp-node-stack`

#### 3. Run Integration Tests

Run Jest tests that invoke Lambda functions and verify Datadog telemetry:

```bash
# Run all tests
npm test

# Run specific test file
npm test -- base-java.test.ts

# Run specific test suite
npm test -- --testNamePattern="OTLP"
```

**Note**: Tests wait several minutes after Lambda invocation to allow telemetry to propagate to Datadog.

### Manual Building (Optional)

If you need to build Lambda functions manually without deploying:

```bash
# Build all functions for a runtime
./scripts/build-java.sh
./scripts/build-dotnet.sh
./scripts/build-python.sh
./scripts/build-node.sh

# Build specific function
./scripts/build-java.sh lambda/otlp-java
``` 


