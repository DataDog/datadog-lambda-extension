import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultNodeLayer,
  defaultNodeRuntime,
} from '../util';

/**
 * Functions that exercise the extension's trace payload-size handling. Add new
 * scenarios as additional functions on this stack.
 */
export class PayloadSize extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    const largeTraceFunctionName = `${id}-large-trace-lambda`;
    const largeTraceFunction = new lambda.Function(this, largeTraceFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/large-trace-node'),
      functionName: largeTraceFunctionName,
      // Headroom for building and flushing a large trace.
      timeout: cdk.Duration.seconds(120),
      memorySize: 1024,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: largeTraceFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
        // Debug level surfaces the extension's payload-size logs the test reads.
        DD_LOG_LEVEL: 'debug',
      },
      logGroup: createLogGroup(this, largeTraceFunctionName),
    });
    largeTraceFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    largeTraceFunction.addLayers(extensionLayer);
    largeTraceFunction.addLayers(nodeLayer);
  }
}
