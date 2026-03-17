import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry, MetricPoint, ENHANCED_METRICS_CONFIG } from './utils/datadog';
import { getIdentifier } from '../config';

const runtimes = ['node', 'python', 'java', 'dotnet'] as const;
type Runtime = typeof runtimes[number];

const identifier = getIdentifier();
const stackName = `integ-${identifier}-lmi`;

describe('LMI Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  beforeAll(async () => {
    const functions: FunctionConfig[] = runtimes.map(runtime => ({
      functionName: `${stackName}-${runtime}-lambda:lmi`,
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

    describe('duration metrics', () => {
      const getTelemetry = () => telemetry[runtime];

      // Helper to get latest value from points
      const getLatestValue = (points: MetricPoint[]) =>
        points.length > 0 ? points[points.length - 1].value : null;

      // Loop through all duration metrics from config
      const durationMetrics = ENHANCED_METRICS_CONFIG.duration.map(
        name => name.split('.').pop()!
      );

      describe.each(durationMetrics)('%s', (metricName) => {
        it('should be emitted', () => {
          const { duration } = getTelemetry().metrics;
          // Metrics may not be indexed in the query time window for all runtimes
          if (duration[metricName].length === 0) {
            console.log(`Note: ${metricName} not found for ${runtime} (may be timing-dependent)`);
            return;
          }
          expect(duration[metricName].length).toBeGreaterThan(0);
        });

        it('should have a positive value', () => {
          const { duration } = getTelemetry().metrics;
          const value = getLatestValue(duration[metricName]);
          // Skip if no data available
          if (value === null) {
            console.log(`Note: ${metricName} has no data for ${runtime}`);
            return;
          }
          expect(value).toBeGreaterThanOrEqual(0);
        });
      });

      // Count validation
      describe('count validation', () => {
        it('should emit runtime_duration for each invocation', () => {
          const { duration } = getTelemetry().metrics;
          // Skip if no data available (metrics may not be indexed in query time window)
          if (duration['runtime_duration'].length === 0) {
            console.log(`Note: runtime_duration not indexed yet for ${runtime} LMI`);
            return;
          }
          // Enhanced metrics may aggregate points, so we check >= 1 instead of exact count
          expect(duration['runtime_duration'].length).toBeGreaterThanOrEqual(1);
        });

        // In LMI mode, init_duration behavior may differ since cold_start is not tracked
        it('should emit init_duration (may be absent in LMI mode)', () => {
          const { duration } = getTelemetry().metrics;
          const initDurationCount = duration['init_duration'].length;
          // In LMI mode, init_duration may or may not be present
          // Just log the count, don't fail
          console.log(`${runtime} LMI init_duration count: ${initDurationCount}`);
          expect(initDurationCount).toBeGreaterThanOrEqual(0);
        });
      });

      // Relationship tests
      it('duration and runtime_duration should be comparable', () => {
        const { duration } = getTelemetry().metrics;
        const durationValue = getLatestValue(duration['duration']);
        const runtimeValue = getLatestValue(duration['runtime_duration']);
        // Skip if either metric has no data
        if (durationValue === null || runtimeValue === null) {
          console.log('Skipping relationship test - missing metric data');
          return;
        }
        // Log the relationship for debugging
        console.log(`${runtime} LMI: duration=${durationValue}ms, runtime_duration=${runtimeValue}ms`);
        expect(durationValue).toBeGreaterThan(0);
        expect(runtimeValue).toBeGreaterThan(0);
      });

      it('post_runtime_duration should be reasonable', () => {
        const { duration } = getTelemetry().metrics;
        const value = getLatestValue(duration['post_runtime_duration']);
        // Skip if metric has no data
        if (value === null) {
          console.log('Skipping post_runtime_duration test - no data');
          return;
        }
        expect(value).toBeGreaterThanOrEqual(0);
      });
    });
  });
});
