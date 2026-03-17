import { LambdaClient, InvokeCommand, TooManyRequestsException, ServiceException } from '@aws-sdk/client-lambda';

const lambdaClient = new LambdaClient({ region: 'us-east-1' });

function formatLambdaError(error: unknown): string {
  if (error instanceof TooManyRequestsException) {
    const parts = [error.name, error.message];
    if (error.Reason) {
      parts.push(`(Reason: ${error.Reason})`);
    }
    if (error.retryAfterSeconds !== undefined) {
      parts.push(`(retryAfterSeconds: ${error.retryAfterSeconds})`);
    }
    return parts.join(' - ');
  }

  if (error instanceof ServiceException) {
    return `${error.name} - ${error.message}`;
  }

  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
}

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
  } catch (error: unknown) {
    console.error(`Lambda invocation failed for '${functionName}': ${formatLambdaError(error)}`);
    throw error;
  }

  const requestId: string = response.$metadata.requestId || '';

  return {
    functionName,
    requestId,
    statusCode: response.StatusCode || 200,
  };
}
