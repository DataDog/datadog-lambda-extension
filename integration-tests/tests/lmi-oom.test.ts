import { invokeLambda } from './utils/lambda';
import { getMetricCount, OUT_OF_MEMORY_METRIC } from './utils/datadog';
import { getIdentifier } from '../config';

/**
 * LMI OOM test.
 *
 * Validates that the `aws.lambda.enhanced.out_of_memory` metric is emitted
 * when an LMI-mode function OOMs. The interesting path is the log-line
 * detector — in LMI mode it tags `Event::OutOfMemory` with `request_id=None`
 * because `platform.start` never fires there, so the metric flows through the
 * no-dedup branch of `Processor::try_increment_oom_metric`.
 *
 * Known dedup gap: in LMI mode the `Runtime.OutOfMemory` path can also fire
 * (via the synthesized runtime-done from `handle_managed_instance_report`),
 * and because it carries `request_id=Some(rid)` it cannot dedup against the
 * log path's `None`. A single OOM may therefore increment the metric more
 * than once. The assertion below is `>= 1` to reflect that — tighten when
 * LMI dedup is addressed.
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
    const windowStart = Date.now();
    await invokeLambda(functionName).catch((err) => {
      throw new Error(`Invoke failed for ${functionName}: ${err}`);
    });

    await sleep(INITIAL_WAIT_MS);

    const deadline = windowStart + TOTAL_BUDGET_MS;
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
