import { Construct } from "constructs";
import * as logs from 'aws-cdk-lib/aws-logs';
import * as cdk from 'aws-cdk-lib';
import * as iam from 'aws-cdk-lib/aws-iam';
import { LayerVersion } from "aws-cdk-lib/aws-lambda";

export const datadogSecretArn = process.env.DATADOG_API_SECRET_ARN!;
export const extensionLayerArn = process.env.EXTENSION_LAYER_ARN!;

export const node20LayerArn = 'arn:aws:lambda:us-east-1:464622532012:layer:Datadog-Node20-x:130';
export const python313LayerArn = 'arn:aws:lambda:us-east-1:464622532012:layer:Datadog-Python313-ARM:117';

export const defaultDatadogEnvVariables = {
    DD_API_KEY_SECRET_ARN: datadogSecretArn,
    DD_SITE: 'datadoghq.com',
    DD_ENV: 'integration',
    DD_VERSION: '1.0.0',
    DD_SERVERLESS_FLUSH_STRATEGY: 'end',
    DD_SERVERLESS_LOGS_ENABLED: 'true',
    DD_LOG_LEVEL: 'info',
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

export const getNode20Layer = (scope: Construct) => {
  return LayerVersion.fromLayerVersionArn(
    scope,
    'DatadogNode20Layer',
    node20LayerArn
  );
};

export const getPython313Layer = (scope: Construct) => {
  return LayerVersion.fromLayerVersionArn(
    scope,
    'DatadogPython313Layer',
    python313LayerArn
  );
};
