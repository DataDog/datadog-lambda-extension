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
 * Failure-accounting test stack.
 *
 * Deploys the same Node function twice: once with the Default (adaptive)
 * flush strategy and once pinned to `end` as a control. Each function has
 * reserved concurrency 1 so sequential invocations share a single execution
 * environment, letting the Default strategy adapt to periodic flushing before
 * the test triggers a timeout or crash.
 */
export class FailureAccounting extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    // Drop the shared `end` override so the extension uses its Default strategy.
    const { DD_SERVERLESS_FLUSH_STRATEGY: _, ...envWithoutFlushStrategy } = defaultDatadogEnvVariables;

    const variants = [
      { name: 'default', env: envWithoutFlushStrategy },
      { name: 'end', env: { ...envWithoutFlushStrategy, DD_SERVERLESS_FLUSH_STRATEGY: 'end' } },
    ];

    for (const variant of variants) {
      const functionName = `${id}-${variant.name}-lambda`;
      const fn = new lambda.Function(this, functionName, {
        runtime: defaultNodeRuntime,
        architecture: lambda.Architecture.ARM_64,
        handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
        code: lambda.Code.fromAsset('./lambda/failure-accounting-node'),
        functionName,
        // Leaves headroom for End-mode flushes, which count against the timeout.
        timeout: cdk.Duration.seconds(10),
        memorySize: 1024,
        reservedConcurrentExecutions: 1,
        environment: {
          ...variant.env,
          DD_SERVICE: functionName,
          DD_TRACE_ENABLED: 'true',
          DD_LAMBDA_HANDLER: 'index.handler',
        },
        logGroup: createLogGroup(this, functionName),
      });
      fn.addToRolePolicy(defaultDatadogSecretPolicy);
      fn.addLayers(extensionLayer);
      fn.addLayers(nodeLayer);
    }
  }
}
