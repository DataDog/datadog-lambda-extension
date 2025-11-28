import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, getPython312Layer } from './util';

export class BasePythonStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const pythonFunctionName = `${id}-python-function`
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: lambda.Runtime.PYTHON_3_12,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/python/lib/python3.12/site-packages/datadog_lambda/handler.handler',
      code: lambda.Code.fromAsset('./lambda/base-python'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...datadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'handler.handler',
      },
      logGroup: createLogGroup(this, pythonFunctionName)
    });
    pythonFunction.addToRolePolicy(secretPolicy)
    pythonFunction.addLayers(getExtensionLayer(this));
    pythonFunction.addLayers(getPython312Layer(this));
  }
}
