import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultNodeLayer,
  defaultNodeRuntime
} from '../util';

export class DurableFunctionStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    const nodeFunctionName = `${id}-node-lambda`;
    const nodeFunction = new lambda.Function(this, nodeFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/durable-node'),
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
