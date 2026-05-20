import * as cdk from 'aws-cdk-lib';
import * as iam from 'aws-cdk-lib/aws-iam';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  datadogSsmParameterArn,
  defaultDatadogSsmPolicy,
  getExtensionLayer,
  getDefaultJavaLayer,
  getDefaultNodeLayer,
  defaultNodeRuntime,
  defaultJavaRuntime,
} from '../util';
import { AUTH_ROLE_NAME } from '../auth-role';

/**
 * CDK Stack for API Key Resolution Integration Tests
 *
 * Exercises each path the extension can use to authenticate to Datadog:
 *   - Delegated auth (on-demand Node + SnapStart Java) - no API key; uses a
 *     shared IAM role from AuthRoleStack so the intake mapping is stable.
 *   - API key sourced from SSM Parameter Store (on-demand Node) - uses its own
 *     default role with ssm:GetParameter on the integration-tests parameter.
 *
 * Additional sources (Secrets Manager, KMS-encrypted env var) can be added
 * here as further Lambda functions.
 */
export class ApiKeyStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const javaLayer = getDefaultJavaLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);

    const role = iam.Role.fromRoleName(this, 'AuthRole', AUTH_ROLE_NAME);

    const orgUuid = '80372b20-c861-11ea-9d80-67f811a2b630';

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

    const ssmAuthEnv = {
      DD_API_KEY_SSM_ARN: datadogSsmParameterArn,
      DD_SITE: 'datadoghq.com',
      DD_ENV: 'integration',
      DD_VERSION: '1.0.0',
      DD_SERVERLESS_FLUSH_STRATEGY: 'end',
      DD_SERVERLESS_LOGS_ENABLED: 'true',
      DD_LOG_LEVEL: 'debug',
      TS: Date.now().toString(),
    };

    const ssmNodeFunctionName = `${id}-ssm-node`;
    const ssmNodeFn = new lambda.Function(this, ssmNodeFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/default-node'),
      functionName: ssmNodeFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...ssmAuthEnv,
        DD_SERVICE: ssmNodeFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, ssmNodeFunctionName),
    });
    ssmNodeFn.addToRolePolicy(defaultDatadogSsmPolicy);
    ssmNodeFn.addLayers(extensionLayer);
    ssmNodeFn.addLayers(nodeLayer);
  }
}
