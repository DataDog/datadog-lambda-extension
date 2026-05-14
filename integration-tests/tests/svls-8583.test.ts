// SVLS-8583: [Tracer-JS] Add basic execution status to aws.lambda span
// Verifies that datadog-lambda-js sets the tag
// `aws_lambda.durable_function.execution_status` on the aws.lambda span when:
//   1. The Lambda event contains a DurableExecutionArn key
//   2. The Lambda result contains a `Status` field with a recognized value
// Also verifies the tag is NOT set for non-durable invocations (guard test).

import { invokeLambda } from './utils/lambda';
import {
  getInvocationTracesLogsByRequestId,
  InvocationTracesLogs,
} from './utils/datadog';
import { getIdentifier } from '../config';
import { DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-svls-8583`;

// A well-formed DurableExecutionArn (the JS guard only checks typeof === 'string',
// but use a realistic ARN for clarity).
const DURABLE_EVENT = {
  DurableExecutionArn:
    'arn:aws:lambda:us-east-1:425362996713:function:test-func:1/durable-execution/my-workflow/exec-12345',
};

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

describe('SVLS-8583: Durable Function Execution Status Tag', () => {
  let succeededInvocStatusCode: number | undefined;
  let failedInvocStatusCode: number | undefined;
  let nonDurableInvocStatusCode: number | undefined;
  let succeededResult: InvocationTracesLogs;
  let failedResult: InvocationTracesLogs;
  let nonDurableResult: InvocationTracesLogs;

  beforeAll(async () => {
    const succeededFunctionName = `${stackName}-durable-succeeded`;
    const failedFunctionName = `${stackName}-durable-failed`;
    const nonDurableFunctionName = `${stackName}-non-durable`;

    // Invoke all three functions concurrently
    const [succeededInvoc, failedInvoc, nonDurableInvoc] = await Promise.all([
      invokeLambda(succeededFunctionName, DURABLE_EVENT),
      invokeLambda(failedFunctionName, DURABLE_EVENT),
      invokeLambda(nonDurableFunctionName, {}),
    ]);

    // Capture invocation status codes for assertions
    succeededInvocStatusCode = succeededInvoc.statusCode;
    failedInvocStatusCode = failedInvoc.statusCode;
    nonDurableInvocStatusCode = nonDurableInvoc.statusCode;

    console.log(`Invoked ${succeededFunctionName}: requestId=${succeededInvoc.requestId}`);
    console.log(`Invoked ${failedFunctionName}: requestId=${failedInvoc.requestId}`);
    console.log(`Invoked ${nonDurableFunctionName}: requestId=${nonDurableInvoc.requestId}`);

    // Wait for Datadog to index traces
    console.log(`Waiting ${DEFAULT_DATADOG_INDEXING_WAIT_MS / 1000}s for Datadog indexing...`);
    await sleep(DEFAULT_DATADOG_INDEXING_WAIT_MS);

    // Collect telemetry for each invocation
    [succeededResult, failedResult, nonDurableResult] = await Promise.all([
      getInvocationTracesLogsByRequestId(succeededFunctionName, succeededInvoc.requestId),
      getInvocationTracesLogsByRequestId(failedFunctionName, failedInvoc.requestId),
      getInvocationTracesLogsByRequestId(nonDurableFunctionName, nonDurableInvoc.requestId),
    ]);

    console.log('All telemetry collected');
  }, 600000);

  describe('durable-succeeded: invoked with DurableExecutionArn, returns Status=SUCCEEDED', () => {
    it('should invoke Lambda successfully', () => {
      expect(succeededInvocStatusCode).toBe(200);
    });

    it('should send exactly one trace to Datadog', () => {
      expect(succeededResult.traces?.length).toBe(1);
    });

    it('should have aws.lambda span with execution_status=SUCCEEDED', () => {
      const trace = succeededResult.traces![0];
      const awsLambdaSpan = trace.spans.find(
        (span: any) => span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();
      expect(awsLambdaSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda',
          custom: {
            aws_lambda: {
              durable_function: {
                execution_status: 'SUCCEEDED',
              },
            },
          },
        },
      });
    });
  });

  describe('durable-failed: invoked with DurableExecutionArn, returns Status=FAILED', () => {
    it('should invoke Lambda successfully', () => {
      expect(failedInvocStatusCode).toBe(200);
    });

    it('should send exactly one trace to Datadog', () => {
      expect(failedResult.traces?.length).toBe(1);
    });

    it('should have aws.lambda span with execution_status=FAILED', () => {
      const trace = failedResult.traces![0];
      const awsLambdaSpan = trace.spans.find(
        (span: any) => span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();
      expect(awsLambdaSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda',
          custom: {
            aws_lambda: {
              durable_function: {
                execution_status: 'FAILED',
              },
            },
          },
        },
      });
    });
  });

  describe('non-durable: invoked without DurableExecutionArn (guard test)', () => {
    it('should invoke Lambda successfully', () => {
      expect(nonDurableInvocStatusCode).toBe(200);
    });

    it('should send exactly one trace to Datadog', () => {
      expect(nonDurableResult.traces?.length).toBe(1);
    });

    it('should NOT have aws_lambda.durable_function.execution_status tag on aws.lambda span', () => {
      const trace = nonDurableResult.traces![0];
      const awsLambdaSpan = trace.spans.find(
        (span: any) => span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();
      const executionStatus =
        awsLambdaSpan?.attributes?.custom?.aws_lambda?.durable_function?.execution_status;
      expect(executionStatus).toBeUndefined();
    });
  });
});
