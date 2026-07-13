// Verifies the invocations metric has durable_function:true only for a durable function, on
// both the cold-start invocation and a subsequent warm invocation.
import { hasMetricWithTag } from './utils/datadog';
import { forceColdStart, invokeLambda } from './utils/lambda';
import { IDENTIFIER, DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';

const stackName = `${IDENTIFIER}-durable-cold-start`;

const INVOCATIONS_METRIC = 'aws.lambda.enhanced.invocations';

describe('Durable Function Cold-Start Metric Tag Integration Tests', () => {
  let coldStartWindowStart: number;
  let coldStartWindowEnd: number;
  let warmInvokeWindowStart: number;
  let warmInvokeWindowEnd: number;

  const durableFunctionName = `${stackName}-durable-lambda`;
  const nonDurableFunctionName = `${stackName}-non-durable-lambda`;

  // Durable functions reject unqualified-ARN invokes; `:$LATEST` avoids publishing a version.
  const invokeBoth = () =>
    Promise.all([
      invokeLambda(`${durableFunctionName}:$LATEST`),
      invokeLambda(nonDurableFunctionName),
    ]);

  const waitForIndexing = () =>
    new Promise((resolve) =>
      setTimeout(resolve, DEFAULT_DATADOG_INDEXING_WAIT_MS),
    );

  beforeAll(async () => {
    const functionNames = [durableFunctionName, nonDurableFunctionName];
    await Promise.all(functionNames.map((fn) => forceColdStart(fn)));

    // Back up 60s so the metric's rollup bucket falls inside the query range.
    coldStartWindowStart = Date.now() - 60_000;
    await invokeBoth(); // invocation #1 (cold start)
    await waitForIndexing();
    coldStartWindowEnd = Date.now();

    warmInvokeWindowStart = Date.now() - 60_000;
    await invokeBoth(); // invocation #2 (warm)
    await waitForIndexing();
    warmInvokeWindowEnd = Date.now();

    console.log('Lambdas invoked twice and indexing waits complete');
  }, 3600000);

  it('durable function cold-start invocation metric should have durable_function:true', async () => {
    const hasTag = await hasMetricWithTag(
      INVOCATIONS_METRIC,
      durableFunctionName,
      'durable_function:true',
      coldStartWindowStart,
      coldStartWindowEnd,
    );
    expect(hasTag).toBe(true);
  });

  it('non-durable function cold-start invocation metric should NOT have durable_function:true', async () => {
    const hasTag = await hasMetricWithTag(
      INVOCATIONS_METRIC,
      nonDurableFunctionName,
      'durable_function:true',
      coldStartWindowStart,
      coldStartWindowEnd,
    );
    expect(hasTag).toBe(false);
  });

  it('durable function warm invocation metric should still have durable_function:true', async () => {
    const hasTag = await hasMetricWithTag(
      INVOCATIONS_METRIC,
      durableFunctionName,
      'durable_function:true',
      warmInvokeWindowStart,
      warmInvokeWindowEnd,
    );
    expect(hasTag).toBe(true);
  });

  it('non-durable function warm invocation metric should still NOT have durable_function:true', async () => {
    const hasTag = await hasMetricWithTag(
      INVOCATIONS_METRIC,
      nonDurableFunctionName,
      'durable_function:true',
      warmInvokeWindowStart,
      warmInvokeWindowEnd,
    );
    expect(hasTag).toBe(false);
  });
});
