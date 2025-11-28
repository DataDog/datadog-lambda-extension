import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import { createLogGroup, datadogEnvVariables, secretPolicy, getExtensionLayer, getDotnetLayer } from './util';

export class BaseDotnetStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const dotnetFunctionName = `${id}-dotnet-function`
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: lambda.Runtime.DOTNET_8,
      architecture: lambda.Architecture.ARM_64,
      handler: 'BaseDotnet::BaseDotnet.Function::FunctionHandler',
      code: lambda.Code.fromAsset('./lambda/base-dotnet'),
      functionName: dotnetFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      environment: {
        ...datadogEnvVariables,
        DD_SERVICE: dotnetFunctionName,
        DD_TRACE_ENABLED: 'true',
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        CORECLR_ENABLE_PROFILING: '1',
        CORECLR_PROFILER: '{846F5F1C-F9AE-4B07-969E-05C26BC060D8}',
        CORECLR_PROFILER_PATH: '/opt/datadog/Datadog.Trace.ClrProfiler.Native.so',
        DD_DOTNET_TRACER_HOME: '/opt/datadog',
      },
      logGroup: createLogGroup(this, dotnetFunctionName)
    });
    dotnetFunction.addToRolePolicy(secretPolicy)
    dotnetFunction.addLayers(getExtensionLayer(this));
    dotnetFunction.addLayers(getDotnetLayer(this));
  }
}
