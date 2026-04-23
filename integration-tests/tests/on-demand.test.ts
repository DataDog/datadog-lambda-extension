import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry, DURATION_METRICS } from './utils/datadog';
import { forceColdStart } from './utils/lambda';
import { getIdentifier } from '../config';

const runtimes = ['node', 'python', 'java', 'dotnet'] as const;
type Runtime = typeof runtimes[number];

const identifier = getIdentifier();
const stackName = `integ-${identifier}-on-demand`;

describe('On-Demand Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  beforeAll(async () => {
    const functions: FunctionConfig[] = runtimes.map(runtime => ({
      functionName: `${stackName}-${runtime}-lambda`,
      runtime,
    }));

    await Promise.all(functions.map(fn => forceColdStart(fn.functionName)));

    telemetry = await invokeAndCollectTelemetry(functions, 2, 1, 5000);

    console.log('All invocations and data fetching completed');
  }, 600000);

  describe.each(runtimes)('%s runtime', (runtime) => {
    const getTelemetry = () => telemetry[runtime];
    const getFirstInvocation = () => getTelemetry()?.threads[0]?.[0];
    const getSecondInvocation = () => getTelemetry()?.threads[0]?.[1];

    describe('first invocation (cold start)', () => {
      it('should invoke Lambda successfully', () => {
        const result = getFirstInvocation();
        expect(result).toBeDefined();
        expect(result.statusCode).toBe(200);
      });

      it('should have "Hello world!" log message', () => {
        const result = getFirstInvocation();
        expect(result).toBeDefined();

        const helloWorldLog = result.logs?.find((log: any) =>
          log.message.includes('Hello world!')
        );
        expect(helloWorldLog).toBeDefined();
      });

      it('should send exactly one trace to Datadog', () => {
        const result = getFirstInvocation();
        expect(result).toBeDefined();
        expect(result.traces?.length).toBe(1);
      });

      it('should have aws.lambda span with cold_start=true', () => {
        const result = getFirstInvocation();
        expect(result).toBeDefined();

        const trace = result.traces![0];
        const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
        );
        expect(awsLambdaSpan).toBeDefined();
        expect(awsLambdaSpan).toMatchObject({
          attributes: {
            operation_name: 'aws.lambda',
            custom: {
              cold_start: 'true'
            }
          }
        });
      });

      if (runtime === 'python') {
        it.failing('[failing] should have aws.lambda.cold_start span', () => {
          const result = getFirstInvocation();
          const trace = result.traces![0];
          const coldStartSpan = trace.spans.find((span: any) =>
            span.attributes.operation_name === 'aws.lambda.cold_start'
          );
          expect(coldStartSpan).toBeDefined();
        });
      } else {
        it('should have aws.lambda.cold_start span', () => {
          const result = getFirstInvocation();
          expect(result).toBeDefined();

          const trace = result.traces![0];
          const coldStartSpan = trace.spans.find((span: any) =>
            span.attributes.operation_name === 'aws.lambda.cold_start'
          );
          expect(coldStartSpan).toBeDefined();
        });
      }
    });

    describe('second invocation (warm start)', () => {
      it('should invoke Lambda successfully', () => {
        const result = getSecondInvocation();
        expect(result).toBeDefined();
        expect(result.statusCode).toBe(200);
      });

      it('should have "Hello world!" log message', () => {
        const result = getSecondInvocation();
        expect(result).toBeDefined();

        const helloWorldLog = result.logs?.find((log: any) =>
          log.message.includes('Hello world!')
        );
        expect(helloWorldLog).toBeDefined();
      });

      it('should send exactly one trace to Datadog', () => {
        const result = getSecondInvocation();
        expect(result).toBeDefined();
        expect(result.traces?.length).toBe(1);
      });

      it('should have aws.lambda span with cold_start=false', () => {
        const result = getSecondInvocation();
        expect(result).toBeDefined();

        const trace = result.traces![0];
        const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
        );
        expect(awsLambdaSpan).toBeDefined();
        expect(awsLambdaSpan).toMatchObject({
          attributes: {
            operation_name: 'aws.lambda',
            custom: {
              cold_start: 'false'
            }
          }
        });
      });

      it('should NOT have aws.lambda.cold_start span', () => {
        const result = getSecondInvocation();
        expect(result).toBeDefined();

        const trace = result.traces![0];
        const coldStartSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda.cold_start'
        );
        expect(coldStartSpan).toBeUndefined();
      });
    });

    describe.skip.each(DURATION_METRICS)('%s', (metric) => {
      it('should have points with positive values', () => {
        const points = getTelemetry().metrics[metric];
        expect(points.length).toBeGreaterThan(0);
        expect(points.every(p => p.value >= 0)).toBe(true);
      });
    });
  });
});
