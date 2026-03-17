import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { getIdentifier } from '../config';

const runtimes = ['node', 'python', 'java', 'dotnet'] as const;
type Runtime = typeof runtimes[number];

const identifier = getIdentifier();
const stackName = `integ-${identifier}-lmi`;

describe('LMI Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  beforeAll(async () => {
    const functions: FunctionConfig[] = runtimes.map(runtime => ({
      functionName: `${stackName}-${runtime}-lambda`,
      runtime,
    }));

    console.log('Invoking LMI functions...');

    // Invoke all LMI functions and collect telemetry
    telemetry = await invokeAndCollectTelemetry(functions, 1);

    console.log('LMI invocation and data fetching completed');
  }, 600000);

  describe.each(runtimes)('%s Runtime with LMI', (runtime) => {
    const getResult = () => telemetry[runtime]?.threads[0]?.[0];

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

    it('aws.lambda.span should have init_type set to lambda-managed-instances', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan?.attributes.custom.init_type).toBe('lambda-managed-instances');
    });

    // SVLS-8232
    // In Managed Instance mode, cold_start span and tag are not sent by bottlecap
    // because the concept is less meaningful with concurrent invocations
    // and it would create a poor flame graph experience due to time gaps.
    //
    // Note: Node and Python tracers (dd-trace-js, dd-trace-py) set their own cold_start
    // attribute on spans independently, so we skip the tag check for those runtimes.
    it('aws.lambda.span should NOT have cold_start span or tag in LMI mode', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];

      // Verify no 'aws.lambda.cold_start' span exists in LMI mode
      const coldStartSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda.cold_start'
      );
      expect(coldStartSpan).toBeUndefined();

      const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();

      // Skip cold_start tag check for Node and Python since their tracer libraries
      // (dd-trace-js, dd-trace-py) set the cold_start attribute independently.
      // Only verify for Java and dotnet where bottlecap controls the span.
      if (runtime !== 'node' && runtime !== 'python') {
        expect(awsLambdaSpan?.attributes.custom.cold_start).toBeUndefined();
      }
    });

    // All duration metrics tests are skipped - metrics indexing is unreliable
    // TODO: Investigate why Datadog metrics API returns inconsistent results
    describe.skip('duration metrics', () => {
      it('should emit aws.lambda.enhanced.runtime_duration', () => {
        const points = telemetry[runtime].metrics.duration['runtime_duration'];
        expect(points.length).toBeGreaterThan(0);
        expect(points[points.length - 1].value).toBeGreaterThan(0);
      });

      it('should emit aws.lambda.enhanced.billed_duration', () => {
        const points = telemetry[runtime].metrics.duration['billed_duration'];
        expect(points.length).toBeGreaterThan(0);
        expect(points[points.length - 1].value).toBeGreaterThan(0);
      });

      it('should emit aws.lambda.enhanced.duration', () => {
        const points = telemetry[runtime].metrics.duration['duration'];
        expect(points.length).toBeGreaterThan(0);
        expect(points[points.length - 1].value).toBeGreaterThan(0);
      });

      it('should emit aws.lambda.enhanced.post_runtime_duration', () => {
        const points = telemetry[runtime].metrics.duration['post_runtime_duration'];
        expect(points.length).toBeGreaterThan(0);
        expect(points[points.length - 1].value).toBeGreaterThanOrEqual(0);
      });

      it('duration should be >= runtime_duration', () => {
        const durationPoints = telemetry[runtime].metrics.duration['duration'];
        const runtimePoints = telemetry[runtime].metrics.duration['runtime_duration'];
        expect(durationPoints.length).toBeGreaterThan(0);
        expect(runtimePoints.length).toBeGreaterThan(0);
        const duration = durationPoints[durationPoints.length - 1].value;
        const runtimeDuration = runtimePoints[runtimePoints.length - 1].value;
        expect(duration).toBeGreaterThanOrEqual(runtimeDuration);
      });
    });
  });
});
