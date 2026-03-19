import * as cdk from 'aws-cdk-lib';
import * as lambda from 'aws-cdk-lib/aws-lambda';
import { LayerVersion } from 'aws-cdk-lib/aws-lambda';
import { Construct } from 'constructs';
import {
  createLogGroup,
  defaultDatadogEnvVariables,
  defaultDatadogSecretPolicy,
  getExtensionLayer,
  getDefaultPythonLayer,
  defaultPythonRuntime,
} from '../util';

// Use custom Python layer ARN if provided, otherwise use default
const customPythonLayerArn = process.env.PYTHON_TRACER_LAYER_ARN;

/**
 * CDK Stack for testing durable_function_execution_status tag.
 *
 * Deploys a Python Lambda function with durable execution enabled.
 * The durable function uses the @durable_execution decorator from the
 * AWS Durable Execution SDK, which causes Lambda to return responses
 * with {"Status": "SUCCEEDED|FAILED|PENDING", ...}.
 *
 * The datadog-lambda-python library extracts this status and adds the
 * `durable_function_execution_status` tag to the aws.lambda span.
 */
export class DurableExecutionStatusStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: cdk.StackProps) {
    super(scope, id, props);

    const extensionLayer = getExtensionLayer(this);
    const pythonLayer = customPythonLayerArn
      ? LayerVersion.fromLayerVersionArn(this, 'CustomPythonLayer', customPythonLayerArn)
      : getDefaultPythonLayer(this);

    const pythonFunctionName = `${id}-python-lambda`;
    const pythonFunction = new lambda.Function(this, pythonFunctionName, {
      runtime: defaultPythonRuntime,
      architecture: lambda.Architecture.ARM_64,
      handler: 'datadog_lambda.handler.handler',
      code: lambda.Code.fromAsset('./lambda/de-status-python/package'),
      functionName: pythonFunctionName,
      timeout: cdk.Duration.seconds(60),
      memorySize: 512,
      environment: {
        ...defaultDatadogEnvVariables,
        DD_SERVICE: pythonFunctionName,
        DD_TRACE_ENABLED: 'true',
        DD_LAMBDA_HANDLER: 'lambda_function.handler',
        DD_TRACE_AGENT_URL: 'http://127.0.0.1:8126',
      },
      logGroup: createLogGroup(this, pythonFunctionName),
      // Enable durable execution
      // executionTimeout must be <= 15 minutes for synchronous invocation
      durableConfig: {
        executionTimeout: cdk.Duration.minutes(15),
        retentionPeriod: cdk.Duration.days(14),
      },
    });
    pythonFunction.addToRolePolicy(defaultDatadogSecretPolicy);
    pythonFunction.addLayers(extensionLayer);
    pythonFunction.addLayers(pythonLayer);
  }
}
