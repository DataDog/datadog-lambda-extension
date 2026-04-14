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

/**
 * Stack for SVLS-8800 integration tests.
 *
 * Tests that aws.lambda.enhanced.invocation is emitted correctly for the
 * first invocation of an On-Demand cold start. Before the fix, the metric
 * was emitted in on_invoke_event before PlatformInitStart fired, causing
 * the durable_function tag to be missing for durable functions.
 * The fix defers emission to on_platform_init_start so the tag is always set.
 */
export class Svls8800Stack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    const nodeFunctionName = `${id}-node-lambda`;
    const nodeFunction = new lambda.Function(this, nodeFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/default-node'),
      functionName: nodeFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: nodeFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, nodeFunctionName),
    });
    nodeFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    nodeFunction.addLayers(extensionLayer);
    nodeFunction.addLayers(nodeLayer);
  }
}
