import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, defaultDatadogEnvVariables, defaultDatadogSecretPolicy, getExtensionLayer } from '../util';

export class BaseJavaStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const javaFunctionName = `${id}-lambda`;

    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: lambda.Runtime.JAVA_11,
      architecture: lambda.Architecture.ARM_64,
      handler: 'Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/base-java', {
        bundling: {
          image: lambda.Runtime.JAVA_11.bundlingImage,
          command: [
            '/bin/sh',
            '-c',
            'cd /asset-input && mvn clean package && cd /asset-output && unzip /asset-input/target/base-java-lambda-1.0.0.jar'
          ]
        }
      }),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: javaFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LOG_LEVEL: 'debug',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126',
      },
      logGroup: createLogGroup(this, javaFunctionName)
    });

    javaFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    javaFunction.addLayers(getExtensionLayer(this));
  }
}
