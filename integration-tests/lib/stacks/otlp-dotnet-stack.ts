import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, defaultDatadogEnvVariables, defaultDatadogSecretPolicy, getExtensionLayer } from '../util';

export class OtlpDotnetStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const dotnetFunctionName = `${id}-lambda`;
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: lambda.Runtime.DOTNET_8,
      architecture: lambda.Architecture.ARM_64,
      handler: 'Function::Function.Handler::FunctionHandler',
      code: lambda.Code.fromAsset('./lambda/otlp-dotnet/bin/function.zip'),
      functionName: dotnetFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: dotnetFunctionName,
        DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT: 'localhost:4318',
        OTEL_EXPORTER_OTLP_ENDPOINT: 'http://localhost:4318',
        OTEL_EXPORTER_OTLP_PROTOCOL: 'http/protobuf',
        OTEL_SERVICE_NAME: dotnetFunctionName,
      },
      logGroup: createLogGroup(this, dotnetFunctionName)
    });

    dotnetFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    dotnetFunction.addLayers(getExtensionLayer(this));
  }
}
