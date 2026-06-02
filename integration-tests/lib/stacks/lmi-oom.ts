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
 * The interesting LMI-specific path: extensions cannot subscribe to `INVOKE` in
 * LMI mode, so `platform.start` is never delivered and
 * `LambdaProcessor::invocation_context.request_id` stays empty. The OOM
 * log-line detector therefore tags `Event::OutOfMemory` with `request_id=None`
 * and `Processor::try_increment_oom_metric` falls into the no-dedup branch.
 *
 * One Python function is enough to exercise this path — `MemoryError` triggers
 * both the runtime-specific log line (path 1) and `Runtime.OutOfMemory` in the
 * synthesized runtime-done event from `handle_managed_instance_report` (path 2).
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
