import * as cdk from "aws-cdk-lib";
import * as lambda from "aws-cdk-lib/aws-lambda";
import { Construct } from "constructs";
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultNodeLayer,
  defaultNodeRuntime,
} from "../util";

/**
 * Two functions that emit the same telemetry (a trace, a custom DogStatsD
 * metric, and logs). The baseline runs with the default config; the standalone
 * function additionally sets DD_APM_STANDALONE_ENABLED=true. The test asserts
 * that traces survive for both, while metrics and logs are suppressed for the
 * standalone function — even though DD_SERVERLESS_LOGS_ENABLED=true is inherited
 * from the default env, which APM standalone mode must override.
 */
export class ApmStandalone extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    const makeFunction = (name: string, extraEnv: Record<string, string>) => {
      const fn = new lambda.Function(this, name, {
        runtime: defaultNodeRuntime,
        architecture: lambda.Architecture.ARM_64,
        handler: "/opt/nodejs/node_modules/datadog-lambda-js/handler.handler",
        code: lambda.Code.fromAsset("./lambda/custom-metrics-node"),
        functionName: name,
        timeout: cdk.Duration.seconds(30),
        memorySize: 256,
        environment: {
          ...defaultDatadogEnvVariables,
          DD_SERVICE: name,
          DD_TRACE_ENABLED: "true",
          DD_LAMBDA_HANDLER: "index.handler",
          ...extraEnv,
        },
        logGroup: createLogGroup(this, name),
      });
      fn.addToRolePolicy(defaultDatadogSecretPolicy);
      fn.addLayers(extensionLayer);
      fn.addLayers(nodeLayer);
      return fn;
    };

    makeFunction(`${id}-baseline-lambda`, {});
    makeFunction(`${id}-standalone-lambda`, {
      DD_APM_STANDALONE_ENABLED: "true",
    });
  }
}
