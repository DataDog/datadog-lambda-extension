import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getNode20Layer,
  getPython313Layer,
  getJava21Layer,
  getDotnet8Layer
} from '../util';

export class Base extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    // Get layers once for the entire stack
    const extensionLayer = getExtensionLayer(this);
    const node20Layer = getNode20Layer(this);
    const python313Layer = getPython313Layer(this);
    const java21Layer = getJava21Layer(this);
    const dotnet8Layer = getDotnet8Layer(this);

    // Node.js Lambda
    const nodeFunctionName = `${id}-node-lambda`;
    const nodeFunction = new lambda.Function(this, nodeFunctionName, {
      runtime: lambda.Runtime.NODEJS_20_X,
      architecture: lambda.Architecture.ARM_64,
      handler: '/opt/nodejs/node_modules/datadog-lambda-js/handler.handler',
      code: lambda.Code.fromAsset('./lambda/base-node'),
      functionName: nodeFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: nodeFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'index.handler',
      },
      logGroup: createLogGroup(this, nodeFunctionName)
    });
    nodeFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    nodeFunction.addLayers(extensionLayer);
    nodeFunction.addLayers(node20Layer);

    // Python Lambda
    const pythonFunctionName = `${id}-python-lambda`;
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: lambda.Runtime.PYTHON_3_13,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/base-python'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
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
    pythonFunction.addLayers(python313Layer);

    // Java Lambda
    const javaFunctionName = `${id}-java-lambda`;
    const javaFunction = new lambda.Function(this, javaFunctionName, {
      runtime: lambda.Runtime.JAVA_21,
      architecture: lambda.Architecture.ARM_64,
      handler: 'example.Handler::handleRequest',
      code: lambda.Code.fromAsset('./lambda/base-java/target/function.jar'),
      functionName: javaFunctionName,
      timeout: cdk.Duration.seconds(30),
      memorySize: 256,
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
    javaFunction.addLayers(java21Layer);

    // .NET Lambda
    const dotnetFunctionName = `${id}-dotnet-lambda`;
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
    dotnetFunction.addLayers(extensionLayer);
    dotnetFunction.addLayers(dotnet8Layer);
  }
}
