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

export class CustomMetrics extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    // Lambda function WITHOUT tag exclusion — all enrichment tags present
    const unfilteredFunctionName = `${id}-unfiltered-lambda`;
    const unfilteredFunction = new lambda.Function(
      this,
      unfilteredFunctionName,
      {
        runtime: defaultNodeRuntime,
        architecture: lambda.Architecture.ARM_64,
        handler: "/opt/nodejs/node_modules/datadog-lambda-js/handler.handler",
        code: lambda.Code.fromAsset("./lambda/custom-metrics-node"),
        functionName: unfilteredFunctionName,
        timeout: cdk.Duration.seconds(30),
        memorySize: 256,
        environment: {
          ...defaultDatadogEnvVariables,
          DD_SERVICE: unfilteredFunctionName,
          DD_TRACE_ENABLED: "true",
          DD_LAMBDA_HANDLER: "index.handler",
        },
        logGroup: createLogGroup(this, unfilteredFunctionName),
      },
    );
    unfilteredFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    unfilteredFunction.addLayers(extensionLayer);
    unfilteredFunction.addLayers(nodeLayer);

    // Lambda function WITH tag exclusion — function_arn and region excluded
    const filteredFunctionName = `${id}-filtered-lambda`;
    const filteredFunction = new lambda.Function(this, filteredFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: "/opt/nodejs/node_modules/datadog-lambda-js/handler.handler",
      code: lambda.Code.fromAsset("./lambda/custom-metrics-node"),
      functionName: filteredFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: filteredFunctionName,
        DD_TRACE_ENABLED: "true",
        DD_LAMBDA_HANDLER: "index.handler",
        DD_CUSTOMER_METRICS_EXCLUDE_TAGS: "function_arn,region",
      },
      logGroup: createLogGroup(this, filteredFunctionName),
    });
    filteredFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    filteredFunction.addLayers(extensionLayer);
    filteredFunction.addLayers(nodeLayer);
  }
}
