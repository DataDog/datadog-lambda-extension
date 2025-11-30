import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, defaultDatadogEnvVariables, defaultDatadogSecretPolicy, getExtensionLayer, getPythonLayer } from '../util';

export class DynamicInstrumentationPythonStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const pythonFunctionName = `${id}-lambda`
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: lambda.Runtime.PYTHON_3_12,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/dynamic-instrumentation-python'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
        DD_LOG_LEVEL: 'debug',
        DD_TRACE_DEBUG: 'true',
        DD_DYNAMIC_INSTRUMENTATION_ENABLED: 'false',
        DD_DYNAMIC_INSTRUMENTATION_PROBE_FILE: '/var/task/probes.json',
      },
      logGroup: createLogGroup(this, pythonFunctionName)
    });
    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy)
    pythonFunction.addLayers(getExtensionLayer(this));
    pythonFunction.addLayers(getPythonLayer(this));
  }
}
