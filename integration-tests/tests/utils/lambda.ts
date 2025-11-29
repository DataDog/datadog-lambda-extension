import { LambdaClient, InvokeCommand, UpdateFunctionConfigurationCommand, GetFunctionConfigurationCommand } from '@aws-sdk/client-lambda';

const lambdaClient = new LambdaClient({ region: 'us-east-1' });

export interface LambdaInvocationResult {
  requestId: string;
  statusCode: number;
  payload: any;
}

export async function invokeLambda(
  functionName: string,
  payload: any = {},
  coldStart: boolean = false
): Promise<LambdaInvocationResult> {
  console.log(`Invoking Lambda: ${functionName}, coldStart: ${coldStart}, payload: ${JSON.stringify(payload)}`);

  if (coldStart) {
    console.log('Forcing cold start...');
    await forceColdStart(functionName);
    console.log('Cold start completed');
  }

  const command = new InvokeCommand({
    FunctionName: functionName,
    Payload: JSON.stringify(payload),
    LogType: 'Tail',
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

  const requestId: string = response.$metadata.requestId || '';

  return {
    requestId,
    statusCode: response.StatusCode || 200,
    payload: responsePayload,
  };
}

export async function forceColdStart(functionName: string): Promise<void> {
  const getConfigCommand = new GetFunctionConfigurationCommand({
    FunctionName: functionName,
  });
  const config = await lambdaClient.send(getConfigCommand);

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

