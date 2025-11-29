import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, getJavaLayer, getJavaCode } from './util';

export class BaseJavaStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const javaFunctionName = `${id}-java-function`
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: lambda.Runtime.JAVA_21,
      architecture: lambda.Architecture.ARM_64,
      handler: 'Handler::handleRequest',
      code: getJavaCode('./lambda/base-java'),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      environment: {
        ...datadogEnvVariables,
        DD_SERVICE: javaFunctionName,
        DD_TRACE_ENABLED: 'true',
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
      },
      logGroup: createLogGroup(this, javaFunctionName)
    });
    javaFunction.addToRolePolicy(secretPolicy)
    javaFunction.addLayers(getExtensionLayer(this));
    javaFunction.addLayers(getJavaLayer(this));
  }
}
