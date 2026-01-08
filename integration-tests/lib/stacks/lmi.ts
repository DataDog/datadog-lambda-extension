import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  setCapacityProvider,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultNodeLayer,
  getDefaultPythonLayer,
  getDefaultJavaLayer,
  getDefaultDotnetLayer,
  defaultNodeRuntime,
  defaultPythonRuntime,
  defaultJavaRuntime,
  defaultDotnetRuntime
} from '../util';

export class LambdaManagedInstancesStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const nodeLayer = getDefaultNodeLayer(this);
    const pythonLayer = getDefaultPythonLayer(this);
    const javaLayer = getDefaultJavaLayer(this);
    const dotnetLayer = getDefaultDotnetLayer(this);

    const nodeFunctionName = `${id}-node-lambda`;
    const nodeFunction = new lambda.Function(this, nodeFunctionName, {
      runtime: defaultNodeRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/lmi-node'),
      functionName: nodeFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 2048,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: nodeFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, nodeFunctionName)
    });
    setCapacityProvider(nodeFunction);
    nodeFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    nodeFunction.addLayers(extensionLayer);
    nodeFunction.addLayers(nodeLayer);
    const nodeAlias = new lambda.Alias(this, `${nodeFunctionName}-alias`, {
      aliasName: 'lmi',
      version: nodeFunction.currentVersion,
    });

    const pythonFunctionName = `${id}-python-lambda`;
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: defaultPythonRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/lmi-python'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 2048,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126'
      },
      logGroup: createLogGroup(this, pythonFunctionName)
    });
    setCapacityProvider(pythonFunction);
    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    pythonFunction.addLayers(extensionLayer);
    pythonFunction.addLayers(pythonLayer);
    const pythonAlias = new lambda.Alias(this, `${pythonFunctionName}-alias`, {
      aliasName: 'lmi',
      version: pythonFunction.currentVersion,
    });

    const javaFunctionName = `${id}-java-lambda`;
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: defaultJavaRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/lmi-java/target/function.jar'),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 2048,
      environment: {
        ...defaultDatadogEnvVariables,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        DD_SERVICE: javaFunctionName,
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, javaFunctionName)
    });
    setCapacityProvider(javaFunction);
    javaFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    javaFunction.addLayers(extensionLayer);
    javaFunction.addLayers(javaLayer);
    const javaAlias = new lambda.Alias(this, `${javaFunctionName}-alias`, {
      aliasName: 'lmi',
      version: javaFunction.currentVersion,
    });

    const dotnetFunctionName = `${id}-dotnet-lambda`;
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: defaultDotnetRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'Function::Function.Handler::FunctionHandler',
      code: lambda.Code.fromAsset('./lambda/lmi-dotnet/bin/function.zip'),
      functionName: dotnetFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 2048,
      environment: {
        ...defaultDatadogEnvVariables,
        AWS_LAMBDA_EXEC_WRAPPER: '/opt/datadog_wrapper',
        DD_SERVICE: dotnetFunctionName,
        DD_TRACE_ENABLED: 'true',
      },
      logGroup: createLogGroup(this, dotnetFunctionName)
    });
    setCapacityProvider(dotnetFunction);
    dotnetFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    dotnetFunction.addLayers(extensionLayer);
    dotnetFunction.addLayers(dotnetLayer);
    const dotnetAlias = new lambda.Alias(this, `${dotnetFunctionName}-alias`, {
      aliasName: 'lmi',
      version: dotnetFunction.currentVersion,
    });
  }
}
