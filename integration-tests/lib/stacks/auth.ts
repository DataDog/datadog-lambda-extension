import * as cdk from 'aws-cdk-lib';
import * as iam from 'aws-cdk-lib/aws-iam';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  getExtensionLayer,
  getDefaultJavaLayer,
  defaultNodeRuntime,
  defaultJavaRuntime,
} from '../util';

/**
 * CDK Stack for Authentication Integration Tests
 *
 * Tests delegated authentication - Lambda uses IAM role to obtain API key.
 * Includes on-demand (Node) and SnapStart (Java) functions.
 *
 * PREREQUISITE: The IAM role ARNs must be configured in Datadog's intake mapping.
 */
export class AuthStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const javaLayer = getDefaultJavaLayer(this);

    const orgUuid = process.env.SERVERLESS_UUID || '';

    const delegatedAuthEnv = {
      DD_SITE: 'datadoghq.com',
      DD_ENV: 'integration',
      DD_VERSION: '1.0.0',
      DD_SERVERLESS_FLUSH_STRATEGY: 'end',
      DD_SERVERLESS_LOGS_ENABLED: 'true',
      DD_LOG_LEVEL: 'debug',
      DD_ORG_UUID: orgUuid,
      TS: Date.now().toString(),
    };

    // Shared IAM role for all auth test functions
    const roleName = `${id}-role`;
    const role = new iam.Role(this, 'ExecutionRole', {
      roleName,
      assumedBy: new iam.ServicePrincipal('lambda.amazonaws.com'),
      managedPolicies: [
        iam.ManagedPolicy.fromAwsManagedPolicyName('service-role/AWSLambdaBasicExecutionRole'),
      ],
    });

    // On-demand Node.js function
    const nodeFunctionName = `${id}-node`;
    const nodeFn = new lambda.Function(this, nodeFunctionName, {
      role,
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'index.handler',
      code: lambda.Code.fromAsset('./lambda/default-node'),
      functionName: nodeFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...delegatedAuthEnv,
        DD_SERVICE: nodeFunctionName,
      },
      logGroup: createLogGroup(this, nodeFunctionName),
    });
    nodeFn.addLayers(extensionLayer);

    // SnapStart Java function
    const javaFunctionName = `${id}-java`;
    const javaFn = new lambda.Function(this, javaFunctionName, {
      role,
      runtime: defaultJavaRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/default-java/target/function.jar'),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      snapStart: lambda.SnapStartConf.ON_PUBLISHED_VERSIONS,
      environment: {
        ...delegatedAuthEnv,
        DD_SERVICE: javaFunctionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, javaFunctionName),
    });
    javaFn.addLayers(extensionLayer);
    javaFn.addLayers(javaLayer);
    const javaVersion = javaFn.currentVersion;
    new lambda.Alias(this, `${javaFunctionName}-snapstart-alias`, {
      aliasName: 'snapstart',
      version: javaVersion,
    });

    new cdk.CfnOutput(this, 'RoleArn', {
      value: role.roleArn,
      description: 'IAM Role ARN - configure in intake mapping',
    });
  }
}
