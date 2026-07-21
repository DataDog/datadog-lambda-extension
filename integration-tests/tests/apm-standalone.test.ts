import {
  getInvocationTracesLogsByRequestId,
  getMetricCount,
  DatadogSpan,
  DatadogTrace,
} from './utils/datadog';
import { forceColdStart, invokeLambda } from './utils/lambda';
import { IDENTIFIER, DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';

const stackName = `${IDENTIFIER}-apm-standalone`;

// Enhanced (billable) metric the extension emits per invocation in normal mode.
const ENHANCED_INVOCATIONS_METRIC = 'aws.lambda.enhanced.invocations';
// Custom DogStatsD metric emitted by the custom-metrics-node handler.
const CUSTOM_METRIC = 'custom.exclude_tags_test';

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

function hasAwsLambdaSpan(traces: DatadogTrace[] | undefined): boolean {
  return (traces ?? [])
    .flatMap((t: DatadogTrace) => t.spans)
    .some((s: DatadogSpan) => s.attributes?.operation_name === 'aws.lambda');
}

describe('APM Standalone Integration Tests', () => {
  const baselineFunctionName = `${stackName}-baseline-lambda`;
  const standaloneFunctionName = `${stackName}-standalone-lambda`;

  let invocationStartTime: number;
  let metricsEndTime: number;
  let baseline: Awaited<ReturnType<typeof getInvocationTracesLogsByRequestId>>;
  let standalone: Awaited<ReturnType<typeof getInvocationTracesLogsByRequestId>>;

  beforeAll(async () => {
    const functionNames = [baselineFunctionName, standaloneFunctionName];

    await Promise.all(functionNames.map(fn => forceColdStart(fn)));

    // Back up the metric query window so the rollup bucket (aligned to the
    // interval boundary, often just before the invocation) falls in range.
    invocationStartTime = Date.now() - 60_000;

    const [baselineInv, standaloneInv] = await Promise.all(
      functionNames.map(fn => invokeLambda(fn)),
    );

    await sleep(DEFAULT_DATADOG_INDEXING_WAIT_MS);
    metricsEndTime = Date.now();

    baseline = await getInvocationTracesLogsByRequestId(
      baselineFunctionName,
      baselineInv.requestId,
    );
    standalone = await getInvocationTracesLogsByRequestId(
      standaloneFunctionName,
      standaloneInv.requestId,
    );

    console.log('Invocation and telemetry collection complete');
  }, 1800000);

  // Traces are unaffected by APM standalone mode — both functions should
  // produce a full trace with the inferred aws.lambda root span.
  describe('traces (preserved in both modes)', () => {
    it('baseline should have the aws.lambda root span', () => {
      expect(hasAwsLambdaSpan(baseline.traces)).toBe(true);
    });

    it('standalone should still have the aws.lambda root span', () => {
      expect(hasAwsLambdaSpan(standalone.traces)).toBe(true);
    });
  });

  describe('enhanced metrics', () => {
    it('baseline should emit aws.lambda.enhanced.invocations', async () => {
      const count = await getMetricCount(
        ENHANCED_INVOCATIONS_METRIC,
        baselineFunctionName,
        invocationStartTime,
        metricsEndTime,
      );
      expect(count).toBeGreaterThan(0);
    });

    it('standalone should NOT emit aws.lambda.enhanced.invocations', async () => {
      const count = await getMetricCount(
        ENHANCED_INVOCATIONS_METRIC,
        standaloneFunctionName,
        invocationStartTime,
        metricsEndTime,
      );
      expect(count).toBe(0);
    });
  });

  describe('custom DogStatsD metrics', () => {
    it('baseline should emit the custom metric', async () => {
      const count = await getMetricCount(
        CUSTOM_METRIC,
        baselineFunctionName,
        invocationStartTime,
        metricsEndTime,
      );
      expect(count).toBeGreaterThan(0);
    });

    it('standalone should NOT emit the custom metric', async () => {
      const count = await getMetricCount(
        CUSTOM_METRIC,
        standaloneFunctionName,
        invocationStartTime,
        metricsEndTime,
      );
      expect(count).toBe(0);
    });
  });

  // Logs must be suppressed even though DD_SERVERLESS_LOGS_ENABLED=true is
  // inherited from the default env — APM standalone mode forces it off.
  describe('logs', () => {
    it('baseline should forward logs to Datadog', () => {
      expect(baseline.logs?.length ?? 0).toBeGreaterThan(0);
    });

    it('standalone should NOT forward any logs to Datadog', () => {
      expect(standalone.logs?.length ?? 0).toBe(0);
    });
  });
});
