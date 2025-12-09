import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getJava21Layer
} from '../util';

export class BaseJavaStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const javaFunctionName = `${id}-lambda`;
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: lambda.Runtime.JAVA_21,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/base-java/target/function.jar'),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: javaFunctionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, javaFunctionName)
    });

    javaFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    javaFunction.addLayers(getExtensionLayer(this));
    javaFunction.addLayers(getJava21Layer(this));
  }
}
