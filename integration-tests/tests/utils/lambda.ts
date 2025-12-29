import { LambdaClient, InvokeCommand, UpdateFunctionConfigurationCommand, GetFunctionConfigurationCommand, PublishVersionCommand } from '@aws-sdk/client-lambda';

const lambdaClient = new LambdaClient({ region: 'us-east-1' });

export interface LambdaInvocationResult {
  requestId: string;
  statusCode: number;
  payload: any;
}

export async function invokeLambda(
  functionName: string,
  payload: any = {},
  coldStart: boolean = false,
  useTailLogs: boolean = true
): Promise<LambdaInvocationResult> {
  console.log(`Invoking Lambda: ${functionName}, coldStart: ${coldStart}, useTailLogs: ${useTailLogs}, payload: ${JSON.stringify(payload)}`);

  if (coldStart) {
    console.log('Forcing cold start...');
    await forceColdStart(functionName);
    console.log('Cold start completed');
  }

  const invokeParams: any = {
    FunctionName: functionName,
    Payload: JSON.stringify(payload),
  };

  // Lambda Managed Instances don't support tail logs
  if (useTailLogs) {
    invokeParams.LogType = 'Tail';
  }

  const command = new InvokeCommand(invokeParams);

  console.log('Sending Lambda invocation request...');
  let response;
  try {
    response = await lambdaClient.send(command);
    console.log(`Lambda invocation completed. StatusCode: ${response.StatusCode}, FunctionError: ${response.FunctionError || 'none'}`);
  } catch (error: any) {
    console.error('Lambda invocation failed:', error.message);
    throw error;
  }

  let responsePayload;
  try {
    responsePayload = JSON.parse(new TextDecoder().decode(response.Payload));
    console.log(`Response payload: ${JSON.stringify(responsePayload)}`);
  } catch (error: any) {
    console.error('Failed to parse response payload:', error.message);
    console.log('Raw payload:', new TextDecoder().decode(response.Payload));
    throw error;
  }

  const requestId: string = response.$metadata.requestId || '';

  return {
    requestId,
    statusCode: response.StatusCode || 200,
    payload: responsePayload,
  };
}

export async function forceColdStart(functionName: string): Promise<void> {
  setTimestampEnvVar(functionName)
  await new Promise(resolve => setTimeout(resolve, 10000));
}


export async function publishVersion(functionName: string): Promise<string> {
  console.debug(`Publishing version for ${functionName}`)
  try {
    await setTimestampEnvVar(functionName);
    await new Promise(resolve => setTimeout(resolve, 10000));
    const command = new PublishVersionCommand({
      FunctionName: functionName,
    });
    const response = await lambdaClient.send(command);
    const version = response.Version || '$LATEST';
    console.debug(`Published version: ${version} for ${functionName}`);
    return version;
  } catch (error: any) {
    console.error('Failed to publish Lambda version:', error.message);
    throw error;
  }
}

export async function waitForSnapStartReady(functionName: string, version: string, timeoutMs: number = 300000): Promise<void> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    try {
      const command = new GetFunctionConfigurationCommand({
        FunctionName: functionName,
        Qualifier: version,
      });
      const config = await lambdaClient.send(command);

      const optimizationStatus = config.SnapStart?.OptimizationStatus;
      const state = config.State;
      const lastUpdateStatus = config.LastUpdateStatus;

      if (optimizationStatus === 'On' && lastUpdateStatus === 'Successful') {
        console.log(`SnapStart ready for ${functionName}:${version}`);
        return;
      }

      await new Promise(resolve => setTimeout(resolve, 10000));
    } catch (error: any) {
      console.error(`Error checking SnapStart status: ${error.message}`);
      await new Promise(resolve => setTimeout(resolve, 10_000));
    }
  }

  throw new Error(`Timeout waiting for SnapStart optimization on ${functionName}:${version}`);
}

export async function setTimestampEnvVar(functionName: string): Promise<void> {
  try {
    const getConfigCommand = new GetFunctionConfigurationCommand({
      FunctionName: functionName,
    });
    const currentConfig = await lambdaClient.send(getConfigCommand);

    const timestamp = Date.now().toString();
    const updatedEnvironment = {
      Variables: {
        ...(currentConfig.Environment?.Variables || {}),
        ts: timestamp,
      },
    };

    const updateConfigCommand = new UpdateFunctionConfigurationCommand({
      FunctionName: functionName,
      Environment: updatedEnvironment,
    });
    await lambdaClient.send(updateConfigCommand);
  } catch (error: any) {
    console.error('Failed to set timestamp environment variable:', error.message);
    throw error;
  }
}