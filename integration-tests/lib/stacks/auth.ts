import * as cdk from 'aws-cdk-lib';
import * as iam from 'aws-cdk-lib/aws-iam';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  getExtensionLayer,
  defaultNodeRuntime,
} from '../util';

/**
 * CDK Stack for Delegated Authentication Integration Tests
 *
 * Tests AWS delegated authentication - Lambda uses IAM role to obtain API key.
 *
 * PREREQUISITE: The IAM role ARN must be configured in Datadog's intake mapping.
 */
export class AuthStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);

    const functionName = id;
    const roleName = `${id}-role`;
    const role = new iam.Role(this, 'ExecutionRole', {
      roleName,
      assumedBy: new iam.ServicePrincipal('lambda.amazonaws.com'),
      managedPolicies: [
        iam.ManagedPolicy.fromAwsManagedPolicyName('service-role/AWSLambdaBasicExecutionRole'),
      ],
    });

    const fn = new lambda.Function(this, functionName, {
      role,
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'index.handler',
      code: lambda.Code.fromAsset('./lambda/default-node'),
      functionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        DD_SITE: 'datadoghq.com',
        DD_ENV: 'integration',
        DD_VERSION: '1.0.0',
        DD_SERVICE: functionName,
        DD_SERVERLESS_FLUSH_STRATEGY: 'end',
        DD_SERVERLESS_LOGS_ENABLED: 'true',
        DD_LOG_LEVEL: 'debug',
        // Delegated auth config
        DD_ORG_UUID: process.env.SERVERLESS_UUID || '',
        DD_DELEGATED_AUTH_ENABLED: 'true',
        TS: Date.now().toString(),
      },
      logGroup: createLogGroup(this, functionName),
    });
    fn.addLayers(extensionLayer);

    new cdk.CfnOutput(this, 'RoleArn', {
      value: fn.role!.roleArn,
      description: 'IAM Role ARN - configure in intake mapping',
    });
  }
}
