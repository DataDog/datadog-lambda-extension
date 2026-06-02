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
 * In LMI mode the function-log JSON payload carries a `requestId` field that
 * the OOM detector reads directly (see `LambdaProcessor::get_message`), so
 * dedup against the other OOM detection paths works reliably even when an
 * OOM fires fast enough that the log line beats `PlatformStart`.
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
