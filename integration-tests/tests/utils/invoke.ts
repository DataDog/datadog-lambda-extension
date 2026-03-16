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

  const requestId: string = response.$metadata.requestId || '';

  return {
    functionName,
    requestId,
    statusCode: response.StatusCode || 200,
  };
}
