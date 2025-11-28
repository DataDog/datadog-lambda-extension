import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, Props, getNode20Layer } from './util';

export class ExampleTestStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: Props) {
    super(scope, id, props);

    const functionName = `exampleTestFunction-${props.identifier}`
    const typescriptFunction = new lambda.Function(this, functionName, {
      runtime: lambda.Runtime.NODEJS_20_X,
      architecture: lambda.Architecture.ARM_64,
      // handler: 'index.handler',
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/example'),
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
    typescriptFunction.addToRolePolicy(secretPolicy)
    typescriptFunction.addLayers(getExtensionLayer(this));
    typescriptFunction.addLayers(getNode20Layer(this));
  }
}
