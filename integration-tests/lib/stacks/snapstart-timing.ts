import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultJavaLayer,
  defaultJavaRuntime
} from '../util';

/**
 * CDK stack for testing SnapStart timestamp adjustment.
 *
 * Creates a Java Lambda function with SnapStart enabled that makes HTTP requests
 * during class initialization. These spans get captured in the SnapStart snapshot
 * and may have stale timestamps after restore. The extension under test should
 * adjust these timestamps to prevent 24+ hour trace durations.
 */
export class SnapstartTiming extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const javaLayer = getDefaultJavaLayer(this);
    const extensionLayer = getExtensionLayer(this);

    const functionName = `${id}-java`;
    const fn = new lambda.Function(this, functionName, {
      runtime: defaultJavaRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/snapstart-timing-java/target/function.jar'),
      functionName: functionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      snapStart: lambda.SnapStartConf.ON_PUBLISHED_VERSIONS,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: functionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, functionName)
    });
    fn.addToRolePolicy(defaultDatadogSecretPolicy);
    fn.addLayers(extensionLayer);
    fn.addLayers(javaLayer);
  }
}
