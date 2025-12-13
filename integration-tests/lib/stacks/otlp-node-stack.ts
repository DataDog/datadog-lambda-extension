import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, defaultDatadogEnvVariables, defaultDatadogSecretPolicy, getExtensionLayer } from '../util';

export class OtlpNodeStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const nodeFunctionName = `${id}-lambda`;
    const nodeFunction = new lambda.Function(this, nodeFunctionName, {
      runtime: lambda.Runtime.NODEJS_20_X,
      architecture: lambda.Architecture.ARM_64,
      handler: 'index.handler',
      code: lambda.Code.fromAsset('./lambda/otlp-node'),
      functionName: nodeFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: nodeFunctionName,
        DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT: 'localhost:4318',
        OTEL_EXPORTER_OTLP_ENDPOINT: 'http://localhost:4318',
        OTEL_EXPORTER_OTLP_PROTOCOL: 'http/protobuf',
        OTEL_SERVICE_NAME: nodeFunctionName,
      },
      logGroup: createLogGroup(this, nodeFunctionName)
    });

    nodeFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    nodeFunction.addLayers(getExtensionLayer(this));
  }
}
