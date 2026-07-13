// Verifies bottlecap holds the cold-start `aws.lambda.enhanced.invocations`
// metric until the Telemetry API's `platform.initStart` event reports the
// runtime, so the metric carries `durable_function:true` for a real
// durable-configured Lambda function (#1301). A plain (non-durable) function
// is invoked alongside as a guard against the tag being set unconditionally.
import { hasMetricWithTag } from './utils/datadog';
import { forceColdStart, invokeLambda } from './utils/lambda';
import { IDENTIFIER, DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';

const stackName = `${IDENTIFIER}-durable-cold-start`;

const INVOCATIONS_METRIC = 'aws.lambda.enhanced.invocations';

describe('Durable Function Cold-Start Metric Tag Integration Tests', () => {
  let invocationStartTime: number;
  let metricsEndTime: number;

  const durableFunctionName = `${stackName}-durable-lambda`;
  const nonDurableFunctionName = `${stackName}-non-durable-lambda`;

  beforeAll(async () => {
    const functionNames = [durableFunctionName, nonDurableFunctionName];

    await Promise.all(functionNames.map((fn) => forceColdStart(fn)));

    // Back up the query window by 60s so the metric bucket (which Datadog
    // aligns to the rollup interval boundary, often before the invocation)
    // falls inside the range we pass to /api/v1/query.
    invocationStartTime = Date.now() - 60_000;

    // Durable functions reject unqualified-ARN invokes ("You cannot invoke a
    // durable function using an unqualified ARN"); `:$LATEST` satisfies that
    // without publishing a version. Metric queries below use the base
    // (unqualified) function name, which is unaffected by this qualifier.
    await Promise.all([
      invokeLambda(`${durableFunctionName}:$LATEST`),
      invokeLambda(nonDurableFunctionName),
    ]);

    await new Promise((resolve) =>
      setTimeout(resolve, DEFAULT_DATADOG_INDEXING_WAIT_MS),
    );

    metricsEndTime = Date.now();

    console.log('Lambdas invoked and indexing wait complete');
  }, 1800000);

  it('durable function cold-start invocation metric should have durable_function:true', async () => {
    const hasTag = await hasMetricWithTag(
      INVOCATIONS_METRIC,
      durableFunctionName,
      'durable_function:true',
      invocationStartTime,
      metricsEndTime,
    );
    expect(hasTag).toBe(true);
  });

  it('non-durable function cold-start invocation metric should NOT have durable_function:true', async () => {
    const hasTag = await hasMetricWithTag(
      INVOCATIONS_METRIC,
      nonDurableFunctionName,
      'durable_function:true',
      invocationStartTime,
      metricsEndTime,
    );
    expect(hasTag).toBe(false);
  });
});
