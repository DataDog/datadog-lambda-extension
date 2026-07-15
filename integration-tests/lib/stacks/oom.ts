import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultNodeLayer,
  getDefaultPythonLayer,
  getDefaultJavaLayer,
  getDefaultDotnetLayer,
  getDefaultRubyLayer,
  defaultNodeRuntime,
  defaultPythonRuntime,
  defaultJavaRuntime,
  defaultDotnetRuntime,
  defaultRubyRuntime,
  defaultGoRuntime,
} from '../util';

/**
 * OOM cross-runtime test stack.
 *
 * Deploys one Lambda per OOM "shape" so the bottlecap dedup change
 * (Context::oom_emitted + try_increment_oom_metric, covering issue #1237)
 * can be exercised end-to-end across every supported runtime. Each function
 * intentionally allocates until it OOMs; the test then asserts the
 * `aws.lambda.enhanced.out_of_memory` metric increments by exactly 1.
 *
 * The detection paths exercised per case:
 *   - oom-node-v8-heap : log-line match `JavaScript heap out of memory`
 *   - oom-node-sigkill : PlatformRuntimeDone `error_type=Runtime.OutOfMemory`
 *   - oom-python       : log line `MemoryError` + PlatformRuntimeDone (dedup)
 *   - oom-ruby         : log line `NoMemoryError` + PlatformRuntimeDone (dedup)
 *   - oom-java         : log line `java.lang.OutOfMemoryError`
 *   - oom-dotnet       : log line `OutOfMemoryException`
 *   - oom-go           : log line `fatal error: runtime: out of memory`
 *                        + PlatformReport memory equality (dedup)
 *
 * Each function is configured with low memory (256 MB, except python at
 * 512 MB) and a short timeout (30 s) so the OOM fires quickly during the
 * integration-test run. See the `oomMemorySize` and `pythonOomMemorySize`
 * comments for the memory rationale.
 */
export class Oom extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);
    const pythonLayer = getDefaultPythonLayer(this);
    const javaLayer = getDefaultJavaLayer(this);
    const dotnetLayer = getDefaultDotnetLayer(this);
    const rubyLayer = getDefaultRubyLayer(this);

    // 256 MB (not the customer's 192 MB from #1237) to give the extension
    // headroom to survive the function's OOM. If memory is too tight the
    // kernel OOM-killer takes the extension instead (Lambda reports
    // `Extension.Crash`), and a dead extension emits neither logs nor the OOM
    // metric. Detection paths are unchanged: functions still hit
    // `max_memory_used == memory_size` and emit runtime-specific OOM log lines.
    const oomMemorySize = 256;
    const oomTimeout = cdk.Duration.seconds(30);

    // python3.13 + the `datadog_lambda` handler shim need more slack: at
    // 256 MB the extension was intermittently killed before its flush,
    // dropping the metric (~1 run in 3). 512 MB keeps it alive.
    const pythonOomMemorySize = 512;

    // Node case A — V8 heap exhaustion (log-line path).
    const nodeV8FunctionName = `${id}-node-v8-heap-lambda`;
    const nodeV8Function = new lambda.Function(this, nodeV8FunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/oom-node-v8-heap'),
      functionName: nodeV8FunctionName,
      timeout: oomTimeout,
      memorySize: oomMemorySize,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: nodeV8FunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
        // Cap V8 heap below the Lambda memory cap so V8 throws its OOM error
        // before the kernel SIGKILLs the process.
        NODE_OPTIONS: '--max-old-space-size=128',
      },
      logGroup: createLogGroup(this, nodeV8FunctionName),
    });
    nodeV8Function.addToRolePolicy(defaultDatadogSecretPolicy);
    nodeV8Function.addLayers(extensionLayer);
    nodeV8Function.addLayers(nodeLayer);

    // Node case B — off-heap Buffer / kernel SIGKILL (PlatformRuntimeDone path).
    const nodeSigkillFunctionName = `${id}-node-sigkill-lambda`;
    const nodeSigkillFunction = new lambda.Function(this, nodeSigkillFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/oom-node-sigkill'),
      functionName: nodeSigkillFunctionName,
      timeout: oomTimeout,
      memorySize: oomMemorySize,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: nodeSigkillFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, nodeSigkillFunctionName),
    });
    nodeSigkillFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    nodeSigkillFunction.addLayers(extensionLayer);
    nodeSigkillFunction.addLayers(nodeLayer);

    // Python — MemoryError; log path and PlatformRuntimeDone path both fire.
    const pythonFunctionName = `${id}-python-lambda`;
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: defaultPythonRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/oom-python'),
      functionName: pythonFunctionName,
      timeout: oomTimeout,
      memorySize: pythonOomMemorySize,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
      },
      logGroup: createLogGroup(this, pythonFunctionName),
    });
    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    pythonFunction.addLayers(extensionLayer);
    pythonFunction.addLayers(pythonLayer);

    // Ruby — NoMemoryError; log path and PlatformRuntimeDone path both fire.
    // Datadog's Ruby tracer is a regular gem (no handler shim like Python's
    // `datadog_lambda.handler.handler`), so the Lambda handler is the user's
    // own `lambda_function.handler` and `DD_LAMBDA_HANDLER` is not used.
    const rubyFunctionName = `${id}-ruby-lambda`;
    const rubyFunction = new lambda.Function(this, rubyFunctionName, {
      runtime: defaultRubyRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'lambda_function.handler',
      code: lambda.Code.fromAsset('./lambda/oom-ruby'),
      functionName: rubyFunctionName,
      timeout: oomTimeout,
      memorySize: oomMemorySize,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: rubyFunctionName,
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, rubyFunctionName),
    });
    rubyFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    rubyFunction.addLayers(extensionLayer);
    rubyFunction.addLayers(rubyLayer);

    // Java — OutOfMemoryError (log-line path).
    const javaFunctionName = `${id}-java-lambda`;
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: defaultJavaRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/oom-java/target/function.jar'),
      functionName: javaFunctionName,
      timeout: oomTimeout,
      memorySize: oomMemorySize,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: javaFunctionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, javaFunctionName),
    });
    javaFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    javaFunction.addLayers(extensionLayer);
    javaFunction.addLayers(javaLayer);

    // .NET — OutOfMemoryException (log-line path).
    const dotnetFunctionName = `${id}-dotnet-lambda`;
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: defaultDotnetRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'Function::Function.Handler::FunctionHandler',
      code: lambda.Code.fromAsset('./lambda/oom-dotnet/bin/function.zip'),
      functionName: dotnetFunctionName,
      timeout: oomTimeout,
      memorySize: oomMemorySize,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: dotnetFunctionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
      },
      logGroup: createLogGroup(this, dotnetFunctionName),
    });
    dotnetFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    dotnetFunction.addLayers(extensionLayer);
    dotnetFunction.addLayers(dotnetLayer);

    // Go — runtime fatal error (log-line path).
    // The Go binary itself is the handler. We don't set
    // AWS_LAMBDA_EXEC_WRAPPER: that wrapper sets language-specific env vars
    // for tracer auto-instrumentation, which Go doesn't use.
    const goFunctionName = `${id}-go-lambda`;
    const goFunction = new lambda.Function(this, goFunctionName, {
      runtime: defaultGoRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'bootstrap',
      code: lambda.Code.fromAsset('./lambda/oom-go/bin'),
      functionName: goFunctionName,
      timeout: oomTimeout,
      memorySize: oomMemorySize,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: goFunctionName,
      },
      logGroup: createLogGroup(this, goFunctionName),
    });
    goFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    goFunction.addLayers(extensionLayer);
    // Go has no tracer layer — the Datadog tracer for Go is a Go module imported
    // into the function source. The extension layer alone is enough for the
    // enhanced metrics this test asserts on.
  }
}
