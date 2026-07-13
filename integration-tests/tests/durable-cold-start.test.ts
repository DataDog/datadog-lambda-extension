// Verifies the invocations metric has durable_function:true only for a durable function, and
// that the tag persists across a second (warm) invocation instead of only being set on cold start.
import { getMetricCount, getMetricCountWithTag } from './utils/datadog';
import { forceColdStart, invokeLambda } from './utils/lambda';
import { IDENTIFIER, DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';

const stackName = `${IDENTIFIER}-durable-cold-start`;

const INVOCATIONS_METRIC = 'aws.lambda.enhanced.invocations';

describe('Durable Function Cold-Start Metric Tag Integration Tests', () => {
  let windowStart: number;
  let windowEnd: number;

  const durableFunctionName = `${stackName}-durable-lambda`;
  const nonDurableFunctionName = `${stackName}-non-durable-lambda`;

  // Durable functions reject unqualified-ARN invokes; `:$LATEST` avoids publishing a version.
  const invokeBoth = () =>
    Promise.all([
      invokeLambda(`${durableFunctionName}:$LATEST`),
      invokeLambda(nonDurableFunctionName),
    ]);

  beforeAll(async () => {
    const functionNames = [durableFunctionName, nonDurableFunctionName];
    await Promise.all(functionNames.map((fn) => forceColdStart(fn)));

    // Back up 60s so the metric's rollup bucket falls inside the query range.
    windowStart = Date.now() - 60_000;

    // Invoke twice back-to-back (cold start, then warm) and wait for indexing once, rather than
    // waiting after each invocation. Whether the tag was set on both invocations (rather than
    // only the first) is checked below by comparing tagged vs. total invocation counts.
    await invokeBoth();
    await invokeBoth();

    await new Promise((resolve) =>
      setTimeout(resolve, DEFAULT_DATADOG_INDEXING_WAIT_MS),
    );
    windowEnd = Date.now();

    console.log('Lambdas invoked twice and indexing wait complete');
  }, 1800000);

  it('durable function invocation metric should have durable_function:true on every invocation', async () => {
    const totalCount = await getMetricCount(
      INVOCATIONS_METRIC,
      durableFunctionName,
      windowStart,
      windowEnd,
    );
    const taggedCount = await getMetricCountWithTag(
      INVOCATIONS_METRIC,
      durableFunctionName,
      'durable_function:true',
      windowStart,
      windowEnd,
    );
    expect(totalCount).toBe(2);
    expect(taggedCount).toBe(totalCount);
  });

  it('non-durable function invocation metric should NOT have durable_function:true on any invocation', async () => {
    const totalCount = await getMetricCount(
      INVOCATIONS_METRIC,
      nonDurableFunctionName,
      windowStart,
      windowEnd,
    );
    const taggedCount = await getMetricCountWithTag(
      INVOCATIONS_METRIC,
      nonDurableFunctionName,
      'durable_function:true',
      windowStart,
      windowEnd,
    );
    expect(totalCount).toBe(2);
    expect(taggedCount).toBe(0);
  });
});
