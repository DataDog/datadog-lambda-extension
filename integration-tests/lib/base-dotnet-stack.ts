import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, getDotnetLayer, getDotnetCode } from './util';

export class BaseDotnetStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const dotnetFunctionName = `${id}-dotnet-function`
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: lambda.Runtime.DOTNET_8,
      architecture: lambda.Architecture.ARM_64,
      handler: 'BaseDotnet::BaseDotnet.Function::FunctionHandler',
      code: getDotnetCode('./lambda/base-dotnet'),
      functionName: dotnetFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      environment: {
        ...datadogEnvVariables,
        DD_SERVICE: dotnetFunctionName,
        DD_TRACE_ENABLED: 'true',
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
      },
      logGroup: createLogGroup(this, dotnetFunctionName)
    });
    dotnetFunction.addToRolePolicy(secretPolicy)
    dotnetFunction.addLayers(getExtensionLayer(this));
    dotnetFunction.addLayers(getDotnetLayer(this));
  }
}
