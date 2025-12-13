import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, defaultDatadogEnvVariables, defaultDatadogSecretPolicy, getExtensionLayer } from '../util';

export class OtlpJavaStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const javaFunctionName = `${id}-lambda`;
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: lambda.Runtime.JAVA_21,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/otlp-java/target/function.jar'),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: javaFunctionName,
        DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT: 'localhost:4318',
        OTEL_EXPORTER_OTLP_ENDPOINT: 'http://localhost:4318',
        OTEL_EXPORTER_OTLP_PROTOCOL: 'http/protobuf',
        OTEL_SERVICE_NAME: javaFunctionName,
      },
      logGroup: createLogGroup(this, javaFunctionName)
    });

    javaFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    javaFunction.addLayers(getExtensionLayer(this));
  }
}
