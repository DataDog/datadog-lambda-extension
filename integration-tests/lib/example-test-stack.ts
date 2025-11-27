import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import * as path from 'path';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, Props } from './util';

export class ExampleTestStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: Props) {
    super(scope, id, props);

    const functionName = `exampleTestFunction-${props.identifier}`
    const typescriptFunction = new lambda.Function(this, functionName, {
      runtime: lambda.Runtime.NODEJS_20_X,
      architecture: lambda.Architecture.ARM_64,
      handler: 'index.handler',
      code: lambda.Code.fromAsset(path.join(__dirname, '../lambda/example')),
      functionName: functionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...datadogEnvVariables,
        DD_SERVICE: functionName,
      },
      logGroup: createLogGroup(this, functionName)
    });
    typescriptFunction.addToRolePolicy(secretPolicy)
    typescriptFunction.addLayers(getExtensionLayer(this));
    
  }
}
