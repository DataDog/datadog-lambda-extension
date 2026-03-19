import * as cdk from 'aws-cdk-lib';
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
export class DelegatedAuthStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);

    // Happy Path Function - Uses delegated auth only
    const happyPathFunctionName = `${id}-happy-path`;
    const happyPathFunction = new lambda.Function(this, happyPathFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'index.handler',
      code: lambda.Code.fromAsset('./lambda/delegated-auth'),
      functionName: happyPathFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        DD_SITE: 'datadoghq.com',
        DD_ENV: 'integration',
        DD_VERSION: '1.0.0',
        DD_SERVICE: happyPathFunctionName,
        DD_SERVERLESS_FLUSH_STRATEGY: 'end',
        DD_SERVERLESS_LOGS_ENABLED: 'true',
        DD_LOG_LEVEL: 'debug',
        // Delegated auth config
        DD_ORG_UUID: '447397',
        DD_DELEGATED_AUTH_ENABLED: 'true',
        TS: Date.now().toString(),
      },
      logGroup: createLogGroup(this, happyPathFunctionName),
    });
    happyPathFunction.addLayers(extensionLayer);

    new cdk.CfnOutput(this, 'HappyPathRoleArn', {
      value: happyPathFunction.role!.roleArn,
      description: 'IAM Role ARN - configure in intake mapping',
    });
  }
}
