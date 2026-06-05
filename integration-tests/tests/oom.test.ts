import { invokeLambda } from './utils/lambda';
import { getMetricCount, OUT_OF_MEMORY_METRIC } from './utils/datadog';
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
 *
 * Ingestion timing: empirical observation in CI is that the
 * `aws.lambda.enhanced.out_of_memory` metric data point is durably ingested
 * within ~30s of the OOM, but Datadog's `/api/v1/query` endpoint sometimes
 * returns no results for very-recently-ingested points (the query engine's
 * snapshot lags the ingest path). The single-shot 5-minute wait used by the
 * other suites is therefore too brittle for this assertion. Instead we poll:
 * after an initial wait we re-query every 30s until every runtime reports
 * count>=1 or the overall budget is exhausted.
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

const INITIAL_WAIT_MS = 90 * 1000;   // wait before first query
const POLL_INTERVAL_MS = 30 * 1000;  // re-query cadence
const TOTAL_BUDGET_MS = 12 * 60 * 1000; // overall ceiling

async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function fetchCounts(start: number, end: number): Promise<Record<string, number>> {
  const results = await Promise.all(
    cases.map(async (c) => ({
      runtime: c.runtime,
      count: await getMetricCount(OUT_OF_MEMORY_METRIC, c.functionName, start, end),
    })),
  );
  return Object.fromEntries(results.map((r) => [r.runtime, r.count]));
}

describe('OOM Integration Tests', () => {
  let countsByRuntime: Record<string, number>;

  beforeAll(async () => {
    const invokeTime = Date.now();
    // Subtract 60s from the query window's lower bound. Datadog rolls OOM
    // metric data points into 10-second buckets aligned to wall-clock
    // multiples and the API only returns buckets whose start timestamp is
    // >= the `from` parameter. If the function OOMs in the same bucket as
    // `invokeTime`, the bucket start (e.g. 19:32:10 for an invoke at
    // 19:32:11.5) is excluded. The `[lmi-oom]` suite hit this on a fast
    // LMI cold start; defensive in this suite too since the timing is
    // workload-dependent.
    const windowStart = invokeTime - 60 * 1000;

    await Promise.all(
      cases.map((c) =>
        invokeLambda(c.functionName).catch((err) => {
          // OOM functions usually succeed at the Invoke API layer (the function
          // is run, just crashes), so a thrown error here is unexpected
          // infrastructure failure rather than the OOM itself.
          throw new Error(`Invoke failed for ${c.functionName}: ${err}`);
        }),
      ),
    );

    await sleep(INITIAL_WAIT_MS);

    const deadline = invokeTime + TOTAL_BUDGET_MS;
    let counts: Record<string, number> = {};
    let attempt = 0;
    while (Date.now() < deadline) {
      attempt++;
      counts = await fetchCounts(windowStart, Date.now());
      const missing = cases.filter((c) => (counts[c.runtime] ?? 0) < 1).map((c) => c.runtime);
      console.log(`OOM poll #${attempt}:`, counts, missing.length ? `(still missing: ${missing.join(', ')})` : '(all runtimes >=1)');
      if (missing.length === 0) {
        break;
      }
      await sleep(POLL_INTERVAL_MS);
    }

    countsByRuntime = counts;
    console.log('OOM counts by runtime (final):', countsByRuntime);
  }, TOTAL_BUDGET_MS + 60 * 1000);

  describe.each(cases)('$runtime runtime', ({ runtime }) => {
    it('should emit exactly one out_of_memory metric for one OOM invocation', () => {
      const count = countsByRuntime[runtime];
      expect(count).toBe(1);
    });
  });
});
