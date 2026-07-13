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

// Durable-function cold-start metric tag test.
// The Lambda Telemetry API only reports a `platform.initStart` runtimeVersion
// containing "DurableFunction" for functions actually configured with
// `durableConfig`, so this stack deploys one durable and one non-durable
// function to exercise bottlecap's `durable_function:true` tagging on the
// cold-start `aws.lambda.enhanced.invocations` metric end-to-end.
export class DurableColdStart extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    const durableFunctionName = `${id}-durable-lambda`;
    const durableFunction = new lambda.Function(this, durableFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/default-node'),
      functionName: durableFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      durableConfig: {
        executionTimeout: cdk.Duration.minutes(5),
      },
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: durableFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, durableFunctionName),
    });
    durableFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    durableFunction.addLayers(extensionLayer);
    durableFunction.addLayers(nodeLayer);

    const nonDurableFunctionName = `${id}-non-durable-lambda`;
    const nonDurableFunction = new lambda.Function(this, nonDurableFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/default-node'),
      functionName: nonDurableFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: nonDurableFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, nonDurableFunctionName),
    });
    nonDurableFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    nonDurableFunction.addLayers(extensionLayer);
    nonDurableFunction.addLayers(nodeLayer);
  }
}
