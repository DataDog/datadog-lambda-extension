import { Construct } from "constructs";
import * as logs from 'aws-cdk-lib/aws-logs';
import * as cdk from 'aws-cdk-lib';
import * as iam from 'aws-cdk-lib/aws-iam';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { LayerVersion } from "aws-cdk-lib/aws-lambda";
import {ACCOUNT, REGION} from "../config";

export const datadogSecretArn = process.env.DATADOG_API_SECRET_ARN!;
export const extensionLayerArn = process.env.EXTENSION_LAYER_ARN!;

export const defaultNodeRuntime = lambda.Runtime.NODEJS_24_X;
export const defaultPythonRuntime = lambda.Runtime.PYTHON_3_13;
export const defaultJavaRuntime = lambda.Runtime.JAVA_21;
export const defaultDotnetRuntime = lambda.Runtime.DOTNET_8;

export const defaultNodeLayerArn = 'arn:aws:lambda:us-east-1:464622532012:layer:Datadog-Node24-x:132';
export const defaultPythonLayerArn = 'arn:aws:lambda:us-east-1:464622532012:layer:Datadog-Python313-ARM:117';
export const defaultJavaLayerArn = 'arn:aws:lambda:us-east-1:464622532012:layer:dd-trace-java:25';
export const defaultDotnetLayerArn = 'arn:aws:lambda:us-east-1:464622532012:layer:dd-trace-dotnet-ARM:23';

export const defaultDatadogEnvVariables = {
    DD_API_KEY_SECRET_ARN: datadogSecretArn,
    DD_SITE: 'datadoghq.com',
    DD_ENV: 'integration',
    DD_VERSION: '1.0.0',
    DD_SERVERLESS_FLUSH_STRATEGY: 'end',
    DD_SERVERLESS_LOGS_ENABLED: 'true',
    DD_LOG_LEVEL: 'info',
    TS: Date.now().toString() // Always forces update when deploying
  };

export const defaultDatadogSecretPolicy = new iam.PolicyStatement({
  effect: iam.Effect.ALLOW,
  actions: [
    'secretsmanager:GetSecretValue',
    'secretsmanager:DescribeSecret',
  ],
  resources: [datadogSecretArn],
});

export const createLogGroup = (scope: Construct, functionName: string) => {
  return new logs.LogGroup(scope, `${functionName}LogGroup`, {
    logGroupName: `/aws/lambda/${functionName}`,
    retention: logs.RetentionDays.ONE_DAY,
    removalPolicy: cdk.RemovalPolicy.DESTROY
  });
};

export const getExtensionLayer = (scope: Construct) => {
  return LayerVersion.fromLayerVersionArn(
    scope,
    'DatadogExtension',
    extensionLayerArn
  );
};

export const getDefaultNodeLayer = (scope: Construct) => {
  return LayerVersion.fromLayerVersionArn(
    scope,
    'DatadogNodeLayer',
    defaultNodeLayerArn
  );
};

export const getDefaultPythonLayer = (scope: Construct) => {
  return LayerVersion.fromLayerVersionArn(
    scope,
    'DatadogPythonLayer',
    defaultPythonLayerArn
  );
};

export const getDefaultJavaLayer = (scope: Construct) => {
  return LayerVersion.fromLayerVersionArn(
    scope,
    'DatadogJavaLayer',
    defaultJavaLayerArn
  );
};

export const getDefaultDotnetLayer = (scope: Construct) => {
  return LayerVersion.fromLayerVersionArn(
    scope,
    'DatadogDotnetLayer',
    defaultDotnetLayerArn
  );
};


export const capacityProviderArn = `arn:aws:lambda:${REGION}:${ACCOUNT}:capacity-provider:integ-default-capacity-provider-cp`;
export function setCapacityProvider(lambdaFunction: lambda.Function) {
    const cfnFunction = lambdaFunction.node.defaultChild as lambda.CfnFunction;
    cfnFunction.addPropertyOverride('CapacityProviderConfig', {
        LambdaManagedInstancesCapacityProviderConfig: {
            CapacityProviderArn: capacityProviderArn
        }
    });
    cfnFunction.addPropertyOverride('FunctionScalingConfig', {
        MinExecutionEnvironments: 1,
        MaxExecutionEnvironments: 1
    });
}