import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { publishVersion, waitForSnapStartReady } from './utils/lambda';
import { getIdentifier } from '../config';

const runtimes = ['java', 'dotnet'] as const;
type Runtime = typeof runtimes[number];

const identifier = getIdentifier();
const stackName = `integ-${identifier}-snapstart`;

describe('Snapstart Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  beforeAll(async () => {
    // Publish new versions and wait for SnapStart optimization
    console.log('Publishing new versions for Snapstart Lambda functions...');
    const versions: Record<Runtime, string> = {} as Record<Runtime, string>;

    for (const runtime of runtimes) {
      const baseFunctionName = `${stackName}-${runtime}-lambda`;
      versions[runtime] = await publishVersion(baseFunctionName);
      console.log(`${runtime} version published: ${versions[runtime]}`);
    }

    console.log('Waiting for SnapStart optimization to complete...');
    for (const runtime of runtimes) {
      const baseFunctionName = `${stackName}-${runtime}-lambda`;
      await waitForSnapStartReady(baseFunctionName, versions[runtime]);
    }
    console.log('All SnapStart versions are ready');

    // Build function configs with version qualifier
    const functions: FunctionConfig[] = runtimes.map(runtime => ({
      functionName: `${stackName}-${runtime}-lambda:${versions[runtime]}`,
      runtime,
    }));

    console.log('Invoking Snapstart Lambda functions...');

    // Invoke with 2 concurrent threads, 2 invocations each
    // - First invocation: restore from snapshot (has snapstart_restore span)
    // - Second invocation: warm (no snapstart_restore span)
    // - 5s delay ensures warm container reuse
    // - 2 threads for trace isolation testing
    telemetry = await invokeAndCollectTelemetry(functions, 2, 2, 5000);

    console.log('All Snapstart Lambda invocations and data fetching completed');
  }, 900000);

  describe.each(runtimes)('%s Runtime with SnapStart', (runtime) => {
    // With concurrency=2, invocations=2:
    // - telemetry[runtime].threads[0][0] = thread 0, first invocation (restore)
    // - telemetry[runtime].threads[0][1] = thread 0, second invocation (warm)
    // - telemetry[runtime].threads[1][0] = thread 1, first invocation (restore)
    // - telemetry[runtime].threads[1][1] = thread 1, second invocation (warm)
    const getRestoreInvocation = () => telemetry[runtime]?.threads[0]?.[0];
    const getWarmInvocation = () => telemetry[runtime]?.threads[0]?.[1];
    const getOtherThreadInvocation = () => telemetry[runtime]?.threads[1]?.[0];

    describe('first invocation (restore from snapshot)', () => {
      it('should invoke successfully', () => {
        const result = getRestoreInvocation();
        expect(result).toBeDefined();
        expect(result.statusCode).toBe(200);
      });

      it('should send one trace to Datadog', () => {
        const result = getRestoreInvocation();
        expect(result).toBeDefined();
        expect(result.traces?.length).toEqual(1);
      });

      it('should have aws.lambda span with init_type=snap-start', () => {
        const result = getRestoreInvocation();
        expect(result).toBeDefined();
        const trace = result.traces![0];
        const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
        );
        expect(awsLambdaSpan).toBeDefined();
        expect(awsLambdaSpan?.attributes.custom.init_type).toBe('snap-start');
      });

      it('should have aws.lambda.snapstart_restore span', () => {
        const result = getRestoreInvocation();
        expect(result).toBeDefined();
        const trace = result.traces![0];
        const restoreSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda.snapstart_restore'
        );
        expect(restoreSpan).toBeDefined();
      });

      it('should NOT have aws.lambda.cold_start span', () => {
        const result = getRestoreInvocation();
        expect(result).toBeDefined();
        const trace = result.traces![0];
        const coldStartSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda.cold_start'
        );
        expect(coldStartSpan).toBeUndefined();
      });
    });

    describe('second invocation (warm)', () => {
      it('should invoke successfully', () => {
        const result = getWarmInvocation();
        expect(result).toBeDefined();
        expect(result.statusCode).toBe(200);
      });

      it('should send one trace to Datadog', () => {
        const result = getWarmInvocation();
        expect(result).toBeDefined();
        expect(result.traces?.length).toEqual(1);
      });

      it('should have aws.lambda span with init_type=snap-start', () => {
        const result = getWarmInvocation();
        expect(result).toBeDefined();
        const trace = result.traces![0];
        const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
        );
        expect(awsLambdaSpan).toBeDefined();
        expect(awsLambdaSpan?.attributes.custom.init_type).toBe('snap-start');
      });

      it('should NOT have aws.lambda.snapstart_restore span', () => {
        const result = getWarmInvocation();
        expect(result).toBeDefined();
        const trace = result.traces![0];
        const restoreSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda.snapstart_restore'
        );
        expect(restoreSpan).toBeUndefined();
      });

      it('should NOT have aws.lambda.cold_start span', () => {
        const result = getWarmInvocation();
        expect(result).toBeDefined();
        const trace = result.traces![0];
        const coldStartSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda.cold_start'
        );
        expect(coldStartSpan).toBeUndefined();
      });
    });

    describe('trace isolation', () => {
      it('should have different trace IDs for all 4 invocations', () => {
        const thread0Restore = telemetry[runtime]?.threads[0]?.[0];
        const thread0Warm = telemetry[runtime]?.threads[0]?.[1];
        const thread1Restore = telemetry[runtime]?.threads[1]?.[0];
        const thread1Warm = telemetry[runtime]?.threads[1]?.[1];

        expect(thread0Restore).toBeDefined();
        expect(thread0Warm).toBeDefined();
        expect(thread1Restore).toBeDefined();
        expect(thread1Warm).toBeDefined();

        const traceIds = [
          thread0Restore.traces![0].trace_id,
          thread0Warm.traces![0].trace_id,
          thread1Restore.traces![0].trace_id,
          thread1Warm.traces![0].trace_id,
        ];

        // All trace IDs should be unique
        const uniqueTraceIds = new Set(traceIds);
        expect(uniqueTraceIds.size).toBe(4);
      });
    });
  });

  // SVLS-5988 - Python SnapStart doesn't completely work as expected.
  // describe('Python Runtime with SnapStart', () => { ... });
});
