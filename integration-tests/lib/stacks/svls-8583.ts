import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultNodeLayer,
  defaultNodeRuntime,
} from '../util';

// Stack for SVLS-8583: durable function execution status tag on the aws.lambda span.
// Three Node.js functions:
//   - durable-succeeded: invoked with DurableExecutionArn event, returns Status=SUCCEEDED
//   - durable-failed:    invoked with DurableExecutionArn event, returns Status=FAILED
//   - non-durable:       invoked without DurableExecutionArn; tag must NOT be set
export class Svls8583Stack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    const commonNodeEnv = {
      ...defaultDatadogEnvVariables,
      DD_TRACE_ENABLED: 'true',
      DD_LAMBDA_HANDLER: 'index.handler',
    };

    // --- durable-succeeded ---
    const succeededName = `${id}-durable-succeeded`;
    const succeededFn = new lambda.Function(this, succeededName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/svls-8583-node'),
      functionName: succeededName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...commonNodeEnv,
        DD_SERVICE: succeededName,
        RETURN_STATUS: 'SUCCEEDED',
      },
      logGroup: createLogGroup(this, succeededName),
    });
    succeededFn.addToRolePolicy(defaultDatadogSecretPolicy);
    succeededFn.addLayers(extensionLayer);
    succeededFn.addLayers(nodeLayer);

    // --- durable-failed ---
    const failedName = `${id}-durable-failed`;
    const failedFn = new lambda.Function(this, failedName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/svls-8583-node'),
      functionName: failedName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...commonNodeEnv,
        DD_SERVICE: failedName,
        RETURN_STATUS: 'FAILED',
      },
      logGroup: createLogGroup(this, failedName),
    });
    failedFn.addToRolePolicy(defaultDatadogSecretPolicy);
    failedFn.addLayers(extensionLayer);
    failedFn.addLayers(nodeLayer);

    // --- non-durable (guard: no DurableExecutionArn in event) ---
    const nonDurableName = `${id}-non-durable`;
    const nonDurableFn = new lambda.Function(this, nonDurableName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/svls-8583-node'),
      functionName: nonDurableName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...commonNodeEnv,
        DD_SERVICE: nonDurableName,
        RETURN_STATUS: 'SUCCEEDED',
      },
      logGroup: createLogGroup(this, nonDurableName),
    });
    nonDurableFn.addToRolePolicy(defaultDatadogSecretPolicy);
    nonDurableFn.addLayers(extensionLayer);
    nonDurableFn.addLayers(nodeLayer);
  }
}
