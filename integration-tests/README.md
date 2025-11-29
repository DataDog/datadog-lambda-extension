# Datadog Lambda Extension Integration Tests

This directory contains integration tests for the Datadog Lambda Extension. 

The general flow is:
1. Deploy test setup using CDK.
2. Invoke lambda functions.
3. Wait ~10 minutes for data to propagate to Datadog.
3. Call Datadog to get telemetry data and check the data based on test requirements.

For simplicity, integraiton tests are setup to only test against ARM runtimes.

## Guidelines

### Naming
* An `identifier` is used to differentiate the different stacks. For local development, the identifier is automatically set using the command `whoami` and parses the user's first name. For gitlab pipelines, the identifier is the git commit short sha.
* Stacks should be named `integ-<identifier>-<stack name>`
* Lambda functions should be named `<stack-id>-<function name>`

### Stacks
* Stacks automatically get deployed as part of the gitlab integration pipeline. Stack should be setup to not retain resources. A helper function `createLogGroup` exists with `removalPolicy: cdk.RemovalPolicy.DESTROY`. 


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
./scripts/publish_local.sh
```

This will create and publish `Datadog-Extension-ARM-<your name>`.

### 2. Deploy Test Stacks

Deploy the CDK stacks that create Lambda functions for testing.

```bash
./scripts/deploy.sh base-node
```

This will create `integ-<your name>-baes-node`. The stacks will use the lambda extension created in the previous step.

### 3. Run Integration Tests

Run Jest tests that invoke Lambda functions and verify Datadog telemetry:

```bash
npm test
```
**Note**: Tests wait 10 minutes after Lambda invocation to allow telemetry to appear in Datadog. 


