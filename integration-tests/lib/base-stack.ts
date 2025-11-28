import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, getNode20Layer } from './util';

export class BaseStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const functionName = `${id}-node-function`
    const nodeFunction = new lambda.Function(this, functionName, {
      runtime: lambda.Runtime.NODEJS_20_X,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/base'),
      functionName: functionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...datadogEnvVariables,
        DD_SERVICE: functionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, functionName)
    });
    nodeFunction.addToRolePolicy(secretPolicy)
    nodeFunction.addLayers(getExtensionLayer(this));
    nodeFunction.addLayers(getNode20Layer(this));
  }
}
