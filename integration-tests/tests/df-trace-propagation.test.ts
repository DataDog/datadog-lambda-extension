import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-durable`;

describe('Durable Function Trace Propagation Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  beforeAll(async () => {
    const functions: FunctionConfig[] = [
      {
        functionName: `${stackName}-node-lambda`,
        runtime: 'node',
      },
    ];

    console.log('Invoking durable function test functions...');

    // Invoke functions and collect telemetry from Datadog
    telemetry = await invokeAndCollectTelemetry(functions, 1);

    console.log('Durable function invocation and data fetching completed');
  }, 600000);

  describe('Node.js runtime with durable function tracing', () => {
    const getResult = () => telemetry['node']?.threads[0]?.[0];

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

    it('should send exactly one trace to Datadog', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.traces?.length).toEqual(1);
    });

    it('trace should have an aws.lambda span', () => {
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

    it('aws.lambda span should have operation_name=aws.lambda', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();
      expect(awsLambdaSpan!.attributes.operation_name).toBe('aws.lambda');
    });
  });
});
