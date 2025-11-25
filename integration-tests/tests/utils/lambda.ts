import { LambdaClient, InvokeCommand, UpdateFunctionConfigurationCommand, GetFunctionConfigurationCommand } from '@aws-sdk/client-lambda';

const lambdaClient = new LambdaClient({ region: 'us-east-1' });

export interface LambdaInvocationResult {
  requestId: string;
  statusCode: number;
  payload: any;
}

/**
 * Invoke a Lambda function
 */
export async function invokeLambda(
  functionName: string,
  payload: any = {},
  coldStart: boolean = false,
  getLogs: boolean = false
): Promise<LambdaInvocationResult> {
  console.log(`Invoking Lambda: ${functionName}, coldStart: ${coldStart}, getLogs: ${getLogs}, payload: ${JSON.stringify(payload)}`);

  if (coldStart) {
    console.log('Forcing cold start...');
    await forceColdStart(functionName);
    console.log('Cold start completed');
  }

  const command = new InvokeCommand({
    FunctionName: functionName,
    Payload: JSON.stringify(payload),
    LogType: 'Tail', // Get last 4KB of logs in response
  });

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

  // Extract requestId from the Lambda function's response payload
  const requestId: string = response.$metadata.requestId || '';
  console.log(`RequestId: ${requestId}`);

  return {
    requestId,
    statusCode: response.StatusCode || 200,
    payload: responsePayload,
  };
}

/**
 * Force a cold start by updating the Lambda function's environment variables
 */
export async function forceColdStart(functionName: string): Promise<void> {
  // Get current configuration
  const getConfigCommand = new GetFunctionConfigurationCommand({
    FunctionName: functionName,
  });
  const config = await lambdaClient.send(getConfigCommand);

  // Update environment variables with a new timestamp to force cold start
  const updateCommand = new UpdateFunctionConfigurationCommand({
    FunctionName: functionName,
    Environment: {
      Variables: {
        ...config.Environment?.Variables,
        TS: Date.now().toString(),
      },
    },
  });

  await lambdaClient.send(updateCommand);

  console.log(`Waiting 10 seconds for Lambda function ${functionName} to reinitialize...`);
  await new Promise(resolve => setTimeout(resolve, 10000));
}

