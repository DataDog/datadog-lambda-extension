import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultPythonLayer,
  getDefaultJavaLayer,
  getDefaultDotnetLayer,
  defaultPythonRuntime,
  defaultJavaRuntime,
  defaultDotnetRuntime
} from '../util';

export class Snapstart extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const pythonLayer = getDefaultPythonLayer(this);
    const javaLayer = getDefaultJavaLayer(this);
    const dotnetLayer = getDefaultDotnetLayer(this);

    const javaFunctionName = `${id}-java-lambda`;
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: defaultJavaRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.SnapstartHandler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/snapstart-java/target/function.jar'),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      snapStart: lambda.SnapStartConf.ON_PUBLISHED_VERSIONS,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: javaFunctionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, javaFunctionName)
    });
    javaFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    javaFunction.addLayers(extensionLayer);
    javaFunction.addLayers(javaLayer);
    const javaVersion = javaFunction.currentVersion;
    const javaAlias = new lambda.Alias(this, `${javaFunctionName}-snapstart-alias`, {
      aliasName: 'snapstart',
      version: javaVersion,
    });

    const pythonFunctionName = `${id}-python-lambda`;
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: defaultPythonRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/snapstart-python'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      snapStart: lambda.SnapStartConf.ON_PUBLISHED_VERSIONS,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126',
        DD_COLD_START_TRACING: 'true',
        DD_MIN_COLD_START_DURATION: '0',
      },
      logGroup: createLogGroup(this, pythonFunctionName)
    });
    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    pythonFunction.addLayers(extensionLayer);
    pythonFunction.addLayers(pythonLayer);
    const pythonVersion = pythonFunction.currentVersion;
    const pythonAlias = new lambda.Alias(this, `${pythonFunctionName}-snapstart-alias`, {
      aliasName: 'snapstart',
      version: pythonVersion,
    });

    const dotnetFunctionName = `${id}-dotnet-lambda`;
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: defaultDotnetRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'Function::Function.SnapstartHandler::FunctionHandler',
      code: lambda.Code.fromAsset('./lambda/snapstart-dotnet/bin/function.zip'),
      functionName: dotnetFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 512,
      snapStart: lambda.SnapStartConf.ON_PUBLISHED_VERSIONS,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: dotnetFunctionName,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
      },
      logGroup: createLogGroup(this, dotnetFunctionName)
    });
    dotnetFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    dotnetFunction.addLayers(extensionLayer);
    dotnetFunction.addLayers(dotnetLayer);
    const dotnetVersion = dotnetFunction.currentVersion;
    const dotnetAlias = new lambda.Alias(this, `${dotnetFunctionName}-snapstart-alias`, {
      aliasName: 'snapstart',
      version: dotnetVersion,
    });
  }
}
