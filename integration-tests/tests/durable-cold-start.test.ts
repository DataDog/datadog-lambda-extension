// Verifies the cold-start invocations metric has durable_function:true only for a durable function.
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

    // Back up 60s so the metric's rollup bucket falls inside the query range.
    invocationStartTime = Date.now() - 60_000;

    // Durable functions reject unqualified-ARN invokes; `:$LATEST` avoids publishing a version.
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
