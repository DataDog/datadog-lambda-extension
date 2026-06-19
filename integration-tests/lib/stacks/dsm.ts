import * as cdk from "aws-cdk-lib";
import * as lambda from "aws-cdk-lib/aws-lambda";
import { Construct } from "constructs";
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultJavaLayer,
  defaultJavaRuntime,
} from "../util";

/**
 * Data Streams Monitoring (DSM) extension-side consume checkpoint test stack.
 *
 * A Java consumer is used deliberately. dd-trace-java's universal
 * instrumentation (enabled via /opt/datadog_wrapper) POSTs the event payload to
 * the extension's /lambda/start-invocation endpoint, which is the only path
 * that drives the extension's DSM extraction hook. The in-process library
 * runtimes (Node/Python via datadog-lambda-*) do NOT call start-invocation and
 * would never exercise the hook.
 *
 * DD_DSM_CONSUME_ENABLED turns the feature on. DD_DATA_STREAMS_ENABLED is forced
 * off so the tracer does not also emit DSM, which would create a second consume
 * node and let the test pass for the wrong reason — the extension must be the
 * only source of `data_streams.latency` for this service.
 *
 * Reuses the shared default-java handler, which accepts an arbitrary JSON event
 * map; the test invokes it with a synthetic SQS event carrying a known producer
 * pathway context.
 */
export class Dsm extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const javaLayer = getDefaultJavaLayer(this);

    const functionName = `${id}-sqs-consumer`;
    const consumer = new lambda.Function(this, functionName, {
      runtime: defaultJavaRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: "example.Handler::handleRequest",
      code: lambda.Code.fromAsset("./lambda/default-java/target/function.jar"),
      functionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: functionName,
        AWS_LAMBDA_EXEC_WRAPPER: "/opt/datadog_wrapper",
        DD_TRACE_ENABLED: "true",
        // Feature under test.
        DD_DSM_CONSUME_ENABLED: "true",
        // Keep tracer-side DSM off so the extension is the only source of
        // data_streams.latency for this service.
        DD_DATA_STREAMS_ENABLED: "false",
      },
      logGroup: createLogGroup(this, functionName),
    });
    consumer.addToRolePolicy(defaultDatadogSecretPolicy);
    consumer.addLayers(extensionLayer);
    consumer.addLayers(javaLayer);
  }
}
