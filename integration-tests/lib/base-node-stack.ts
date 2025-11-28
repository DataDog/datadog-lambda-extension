import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, getNode20Layer } from './util';

export class BaseNodeStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const nodeFunctionName = `${id}-node-function`
    const nodeFunction = new lambda.Function(this, nodeFunctionName, {
      runtime: lambda.Runtime.NODEJS_20_X,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/base-node'),
      functionName: nodeFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...datadogEnvVariables,
        DD_SERVICE: nodeFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, nodeFunctionName)
    });
    nodeFunction.addToRolePolicy(secretPolicy)
    nodeFunction.addLayers(getExtensionLayer(this));
    nodeFunction.addLayers(getNode20Layer(this));
  }
}
