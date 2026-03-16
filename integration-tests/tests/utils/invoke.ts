import { LambdaClient, InvokeCommand } from '@aws-sdk/client-lambda';

const lambdaClient = new LambdaClient({ region: 'us-east-1' });

export interface InvocationResult {
  functionName: string;
  requestId: string;
  statusCode?: number;
}

export async function invokeLambda(
  functionName: string,
  payload: any = {},
): Promise<InvocationResult> {
  const command = new InvokeCommand({
    FunctionName: functionName,
    Payload: JSON.stringify(payload),
  });

  let response;
  try {
    response = await lambdaClient.send(command);
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
    functionName,
    requestId,
    statusCode: response.StatusCode || 200,
  };
}
