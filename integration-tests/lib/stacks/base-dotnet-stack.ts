import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDotnet8Layer
} from '../util';

export class BaseDotnetStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const dotnetFunctionName = `${id}-lambda`;
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: lambda.Runtime.DOTNET_8,
      architecture: lambda.Architecture.ARM_64,
      handler: 'Function::Function.Handler::FunctionHandler',
      code: lambda.Code.fromAsset('./lambda/base-dotnet/bin/function.zip'),
      functionName: dotnetFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: dotnetFunctionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
      },
      logGroup: createLogGroup(this, dotnetFunctionName)
    });

    dotnetFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    dotnetFunction.addLayers(getExtensionLayer(this));
    dotnetFunction.addLayers(getDotnet8Layer(this));
  }
}
