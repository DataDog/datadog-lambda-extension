import { invokeLambda } from './utils/lambda';
import { getMetricCount, OUT_OF_MEMORY_METRIC } from './utils/datadog';
import { DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';
import { getIdentifier } from '../config';

/**
 * Cross-runtime OOM test.
 *
 * Each function is intentionally configured to OOM on its first invocation.
 * Bottlecap has three detection paths that can fire for the same invocation
 * (runtime-specific log line, `Runtime.OutOfMemory` `error_type` in
 * `PlatformRuntimeDone`, `max_memory_used_mb == memory_size_mb` in
 * `PlatformReport`); the `Context::oom_emitted` flag introduced for #1237
 * dedupes them so the metric increments exactly once per invocation.
 *
 * The Python/Ruby/Go cases are particularly meaningful regressions because
 * they trigger more than one detection path naturally — if dedup is broken,
 * those counts go to 2.
 */
const identifier = getIdentifier();
const stackName = `integ-${identifier}-oom`;

interface OomCase {
  runtime: string;
  functionName: string;
}

const cases: OomCase[] = [
  { runtime: 'node-v8-heap', functionName: `${stackName}-node-v8-heap-lambda` },
  { runtime: 'node-sigkill', functionName: `${stackName}-node-sigkill-lambda` },
  { runtime: 'python',       functionName: `${stackName}-python-lambda` },
  { runtime: 'ruby',          functionName: `${stackName}-ruby-lambda` },
  { runtime: 'java',          functionName: `${stackName}-java-lambda` },
  { runtime: 'dotnet',        functionName: `${stackName}-dotnet-lambda` },
  { runtime: 'go',            functionName: `${stackName}-go-lambda` },
];

async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe('OOM Integration Tests', () => {
  let countsByRuntime: Record<string, number>;
  let windowStart: number;
  let windowEnd: number;

  // Invoke every function once, wait for Datadog to ingest, then query once
  // for each. Keeping invocations and the query inside `beforeAll` lets each
  // per-runtime test below assert against the same data set.
  beforeAll(async () => {
    windowStart = Date.now();

    await Promise.all(
      cases.map((c) =>
        invokeLambda(c.functionName).catch((err) => {
          // OOM functions usually succeed at the Invoke API layer (the function
          // is run, just crashes), so a thrown error here is unexpected
          // infrastructure failure rather than the OOM itself. Re-throw so the
          // test surfaces it.
          throw new Error(`Invoke failed for ${c.functionName}: ${err}`);
        }),
      ),
    );

    await sleep(DEFAULT_DATADOG_INDEXING_WAIT_MS);
    windowEnd = Date.now();

    const results = await Promise.all(
      cases.map(async (c) => ({
        runtime: c.runtime,
        count: await getMetricCount(
          OUT_OF_MEMORY_METRIC,
          c.functionName,
          windowStart,
          windowEnd,
        ),
      })),
    );

    countsByRuntime = Object.fromEntries(results.map((r) => [r.runtime, r.count]));
    console.log('OOM counts by runtime:', countsByRuntime);
  }, 10 * 60 * 1000);

  describe.each(cases)('$runtime runtime', ({ runtime }) => {
    it('should emit exactly one out_of_memory metric for one OOM invocation', () => {
      const count = countsByRuntime[runtime];
      expect(count).toBe(1);
    });
  });
});
