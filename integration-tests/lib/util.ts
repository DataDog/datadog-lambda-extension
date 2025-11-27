import { Construct } from "constructs";
import * as logs from 'aws-cdk-lib/aws-logs';
import * as cdk from 'aws-cdk-lib';
import * as iam from 'aws-cdk-lib/aws-iam';
import { LayerVersion } from "aws-cdk-lib/aws-lambda";

export const datadogSecretArn = process.env.DATADOG_API_SECRET_ARN || '';
// export const extensionLayerArn = process.env.EXTENSION_LAYER_ARN || '';
export const extensionLayerArn = 'arn:aws:lambda:us-east-1:464622532012:layer:Datadog-Extension-ARM:89' 

export interface Props extends cdk.StackProps{
  suffix: string
}

export const datadogEnvVariables = {
    DD_API_KEY_SECRET_ARN: datadogSecretArn,
    DD_SITE: 'datadoghq.com',
    DD_ENV: 'sandbox',
    DD_VERSION: '1.0.0',
    DD_SERVERLESS_FLUSH_STRATEGY: 'end',
    DD_SERVERLESS_LOGS_ENABLED: 'true',
    DD_LOG_LEVEL: 'debug',
  };

export const secretPolicy = new iam.PolicyStatement({
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
