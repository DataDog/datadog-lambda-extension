import * as os from 'os';

export const ACCOUNT = process.env.CDK_DEFAULT_ACCOUNT || process.env.AWS_ACCOUNT_ID;
export const REGION = process.env.CDK_DEFAULT_REGION || process.env.AWS_REGION || 'us-east-1';

// Default wait time for Datadog to index logs and traces after Lambda invocation
export const DEFAULT_DATADOG_INDEXING_WAIT_MS = 5 * 60 * 1000; // 5 minutes

// Extended wait time for async invocations (SQS, SNS) - need more time for message processing
export const ASYNC_DATADOG_INDEXING_WAIT_MS = 90 * 1000; // 90 seconds

// Extended wait time for tests that need more time (e.g., OTLP tests)
export const DATADOG_INDEXING_WAIT_5_MIN_MS = 5 * 60 * 1000; // 5 minutes

// Extra wait applied only to .NET when its trace hasn't landed yet after the default wait.
// The .NET tracer's CLR profiler bootstrap (AWS_LAMBDA_EXEC_WRAPPER + JIT) is slower and more
// variable than Node/Python/Java, so .NET traces intermittently miss the default indexing window.
export const DOTNET_EXTRA_INDEXING_WAIT_MS = 3 * 60 * 1000; // 3 minutes


export function getIdentifier(): string {
  if (process.env.IDENTIFIER) {
    return process.env.IDENTIFIER;
  }

  try {
    const username = os.userInfo().username;
    const firstName = username.split('.')[0];
    if (firstName && firstName.length > 0) {
      return `it-${firstName}`;
    }
  } catch (error) {
    console.error('Error getting identifier:', error);
  }

  return 'it-integration';
}

export const IDENTIFIER = getIdentifier();
