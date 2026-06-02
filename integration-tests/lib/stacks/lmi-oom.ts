import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  setCapacityProvider,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultPythonLayer,
  defaultPythonRuntime,
} from '../util';

/**
 * LMI OOM test stack.
 *
 * Exercises bottlecap OOM detection on a Lambda Managed Instance (LMI) function.
 * Verified empirically (PR #1241): when a Python `MemoryError` fires
 * immediately on first allocation, the function's OOM log line is processed
 * by `LambdaProcessor` before its `PlatformStart` handler sets
 * `invocation_context.request_id`, so `current_request_id()` returns `None`
 * and the OOM metric flows through the no-dedup branch of
 * `Processor::try_increment_oom_metric`.
 */
export class LmiOom extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const pythonLayer = getDefaultPythonLayer(this);

    const functionName = `${id}-python-lambda`;
    const fn = new lambda.Function(this, functionName, {
      runtime: defaultPythonRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/oom-python'),
      functionName: functionName,
      timeout: cdk.Duration.seconds(30),
      // 256 MB — see `oom.ts` for why we don't use the customer's 192 MB
      // (kernel OOM-kills the extension itself otherwise).
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: functionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126',
      },
      logGroup: createLogGroup(this, functionName),
    });
    setCapacityProvider(fn);
    fn.addToRolePolicy(defaultDatadogSecretPolicy);
    fn.addLayers(extensionLayer);
    fn.addLayers(pythonLayer);
  }
}
