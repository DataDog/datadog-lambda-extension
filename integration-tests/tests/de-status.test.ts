import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-de-status`;

describe('Durable Execution Status Tag Integration Tests', () => {
  let results: Record<string, DatadogTelemetry[][]>;

  beforeAll(async () => {
    // Durable functions require a qualified ARN (with version) for invocation
    const functions: FunctionConfig[] = [
      {
        functionName: `${stackName}-python-lambda:$LATEST`,
        runtime: 'python',
      },
    ];

    console.log('Invoking durable execution functions...');

    // Invoke all durable execution functions and collect telemetry
    results = await invokeAndCollectTelemetry(functions, 1);

    console.log('Durable execution invocation and data fetching completed');
  }, 600000);

  describe('Python Runtime with Durable Execution', () => {
    const getResult = () => results['python']?.[0]?.[0];

    it('should invoke Lambda successfully', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should have logs in Datadog', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.logs).toBeDefined();
      expect(result.logs!.length).toBeGreaterThan(0);
    });

    it('should have "Hello world!" log message', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.logs).toBeDefined();
      const helloWorldLog = result.logs!.find((log: any) =>
        log.message && log.message.includes('Hello world!')
      );
      expect(helloWorldLog).toBeDefined();
    });

    it('should send one trace to Datadog', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.traces?.length).toEqual(1);
    });

    it('trace should have exactly one span with operation_name=aws.lambda', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      expect(trace.spans).toBeDefined();

      const awsLambdaSpans = trace.spans.filter((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpans).toBeDefined();
      expect(awsLambdaSpans.length).toEqual(1);
    });

    it('aws.lambda span should have durable_function_execution_status tag', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();

      // The durable_function_execution_status tag should be present
      const status = awsLambdaSpan?.attributes.custom?.durable_function_execution_status;
      expect(status).toBeDefined();
    });

    it('durable_function_execution_status tag should be one of: SUCCEEDED, FAILED, STOPPED, TIMED_OUT', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();

      const status = awsLambdaSpan?.attributes.custom?.durable_function_execution_status;
      expect(status).toBeDefined();
      expect(['SUCCEEDED', 'FAILED', 'STOPPED', 'TIMED_OUT']).toContain(status);
    });

    it('durable_function_execution_status tag should be SUCCEEDED for successful execution', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();

      // For a successful durable function invocation that completes in one call,
      // the status should be SUCCEEDED
      const status = awsLambdaSpan?.attributes.custom?.durable_function_execution_status;
      expect(status).toBe('SUCCEEDED');
    });
  });
});
