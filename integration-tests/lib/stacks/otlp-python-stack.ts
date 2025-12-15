import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, defaultDatadogEnvVariables, defaultDatadogSecretPolicy, getExtensionLayer } from '../util';

export class OtlpPythonStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const pythonFunctionName = `${id}-lambda`;

    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: lambda.Runtime.PYTHON_3_12,
      architecture: lambda.Architecture.ARM_64,
      handler: 'lambda_function.handler',
      code: lambda.Code.fromAsset('./lambda/otlp-python/package'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT: 'localhost:4318',
        OTEL_EXPORTER_OTLP_ENDPOINT: 'http://localhost:4318',
        OTEL_EXPORTER_OTLP_PROTOCOL: 'http/protobuf',
        OTEL_SERVICE_NAME: pythonFunctionName,
      },
      logGroup: createLogGroup(this, pythonFunctionName)
    });

    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    pythonFunction.addLayers(getExtensionLayer(this));
  }
}
