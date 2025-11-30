import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, defaultDatadogEnvVariables, defaultDatadogSecretPolicy, getExtensionLayer, getPython313Layer } from '../util';

export class BasePythonStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const pythonFunctionName = `${id}-lambda`
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: lambda.Runtime.PYTHON_3_13,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/base-python'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
        DD_LOG_LEVEL: 'debug',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126',
        DD_COLD_START_TRACING: 'true',
        DD_MIN_COLD_START_DURATION: '0',
      },
      logGroup: createLogGroup(this, pythonFunctionName)
    });
    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy)
    pythonFunction.addLayers(getExtensionLayer(this));
    pythonFunction.addLayers(getPython313Layer(this));
  }
}
