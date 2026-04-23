import * as os from 'os';

export const ACCOUNT = process.env.CDK_DEFAULT_ACCOUNT || process.env.AWS_ACCOUNT_ID;
export const REGION = process.env.CDK_DEFAULT_REGION || process.env.AWS_REGION || 'us-east-1';

// Default wait time for Datadog to index logs and traces after Lambda invocation
export const DEFAULT_DATADOG_INDEXING_WAIT_MS = 5 * 60 * 1000; // 5 minutes

// Extended wait time for async invocations (SQS, SNS) - need more time for message processing
export const ASYNC_DATADOG_INDEXING_WAIT_MS = 90 * 1000; // 90 seconds

// Extended wait time for tests that need more time (e.g., OTLP tests)
export const DATADOG_INDEXING_WAIT_5_MIN_MS = 5 * 60 * 1000; // 5 minutes


export function getIdentifier(): string {
  if (process.env.IDENTIFIER) {
    return process.env.IDENTIFIER;
  }

  try {
    const username = os.userInfo().username;
    const firstName = username.split('.')[0];
    if (firstName && firstName.length > 0) {
      return firstName;
    }
  } catch (error) {
    console.error('Error getting identifier:', error);
  }

  return 'integration';
}
