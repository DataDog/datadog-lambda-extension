import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultPythonLayer,
  defaultPythonRuntime,
} from '../util';

export class Durable extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const pythonLayer = getDefaultPythonLayer(this);

    // Non-durable Python Lambda - used to verify that logs do NOT have durable execution context.
    const pythonFunctionName = `${id}-python-lambda`;
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: defaultPythonRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/durable-python'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126',
        DD_LOG_LEVEL: 'DEBUG',
      },
      logGroup: createLogGroup(this, pythonFunctionName),
    });
    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    pythonFunction.addLayers(extensionLayer);
    pythonFunction.addLayers(pythonLayer);

    // Durable Python Lambda - used to verify that logs DO have durable execution context.
    const durablePythonFunctionName = `${id}-python-durable-lambda`;
    const durablePythonFunction = new lambda.Function(this, durablePythonFunctionName, {
      runtime: defaultPythonRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/durable-python'),
      functionName: durablePythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: durablePythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_durable_function.handler',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126',
        DD_LOG_LEVEL: 'DEBUG',
      },
      logGroup: createLogGroup(this, durablePythonFunctionName),
      durableConfig: {
        executionTimeout: cdk.Duration.minutes(15),
        retentionPeriod: cdk.Duration.days(14),
      },
    });
    durablePythonFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    durablePythonFunction.addLayers(extensionLayer);
    durablePythonFunction.addLayers(pythonLayer);
  }
}
