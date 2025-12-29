import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  setCapacityProvider,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getNode24Layer,
  getPython313Layer,
  getJava21Layer,
  getDotnet8Layer
} from '../util';

export class LambdaManagedInstancesStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const node24Layer = getNode24Layer(this);

    const nodeFunctionName = `${id}-node-lambda`;
    const nodeFunction = new lambda.Function(this, nodeFunctionName, {
      runtime: lambda.Runtime.NODEJS_24_X,
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
    nodeFunction.addLayers(node24Layer);
    const nodeAlias = new lambda.Alias(this, `${nodeFunctionName}-alias`, {
      aliasName: 'lmi',
      version: nodeFunction.currentVersion,
    });

    const pythonFunctionName = `${id}-python-lambda`;
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: lambda.Runtime.PYTHON_3_13,
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
    pythonFunction.addLayers(getPython313Layer(this));
    const pythonAlias = new lambda.Alias(this, `${pythonFunctionName}-alias`, {
      aliasName: 'lmi',
      version: pythonFunction.currentVersion,
    });

    const javaFunctionName = `${id}-java-lambda`;
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: lambda.Runtime.JAVA_21,
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
    javaFunction.addLayers(getJava21Layer(this));
    const javaAlias = new lambda.Alias(this, `${javaFunctionName}-alias`, {
      aliasName: 'lmi',
      version: javaFunction.currentVersion,
    });

    const dotnetFunctionName = `${id}-dotnet-lambda`;
    const dotnetFunction = new lambda.Function(this, dotnetFunctionName, {
      runtime: lambda.Runtime.DOTNET_8,
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
    dotnetFunction.addLayers(getDotnet8Layer(this));
    const dotnetAlias = new lambda.Alias(this, `${dotnetFunctionName}-alias`, {
      aliasName: 'lmi',
      version: dotnetFunction.currentVersion,
    });
  }
}
