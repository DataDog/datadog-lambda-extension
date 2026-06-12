import { invokeLambda } from './utils/lambda';
import { getMetricCount, OUT_OF_MEMORY_METRIC } from './utils/datadog';
import { getIdentifier } from '../config';

/**
 * LMI OOM test.
 *
 * Validates that the `aws.lambda.enhanced.out_of_memory` metric is emitted
 * when an LMI-mode Python function hits `MemoryError`. In LMI mode the OOM
 * log path tags `Event::OutOfMemory` with the `requestId` parsed from the
 * function-log JSON payload, so dedup works without depending on
 * `PlatformStart` having raced ahead of the log line.
 *
 * Asserts `>= 1` rather than `== 1` to stay robust against other paths firing
 * (e.g. a future change where `handle_managed_instance_report` surfaces
 * `Runtime.OutOfMemory` in the synthesized runtime-done).
 */
const identifier = getIdentifier();
const stackName = `integ-${identifier}-lmi-oom`;
const functionName = `${stackName}-python-lambda`;

const INITIAL_WAIT_MS = 90 * 1000;
const POLL_INTERVAL_MS = 30 * 1000;
const TOTAL_BUDGET_MS = 12 * 60 * 1000;

async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe('LMI OOM Integration Test', () => {
  let count = 0;

  beforeAll(async () => {
    const invokeTime = Date.now();
    // Subtract 60s from the query window's lower bound. Datadog rolls OOM
    // metric data points into 10-second buckets aligned to wall-clock
    // multiples; the bucket containing the OOM event is timestamped at the
    // bucket *start*, and the query API only returns buckets whose start
    // is >= the `from` parameter. If `windowStart == invokeTime` and the
    // function OOMs in the same 10-second bucket (e.g. invoke 19:32:11.5,
    // OOM 19:32:18 → bucket 19:32:10), the bucket would be excluded.
    const windowStart = invokeTime - 60 * 1000;
    await invokeLambda(functionName).catch((err) => {
      throw new Error(`Invoke failed for ${functionName}: ${err}`);
    });

    await sleep(INITIAL_WAIT_MS);

    const deadline = invokeTime + TOTAL_BUDGET_MS;
    let attempt = 0;
    while (Date.now() < deadline) {
      attempt++;
      count = await getMetricCount(OUT_OF_MEMORY_METRIC, functionName, windowStart, Date.now());
      console.log(`LMI OOM poll #${attempt}: count=${count}`);
      if (count >= 1) {
        break;
      }
      await sleep(POLL_INTERVAL_MS);
    }
    console.log(`LMI OOM count (final): ${count}`);
  }, TOTAL_BUDGET_MS + 60 * 1000);

  it('should emit at least one out_of_memory metric for one OOM invocation in LMI mode', () => {
    expect(count).toBeGreaterThanOrEqual(1);
  });
});
