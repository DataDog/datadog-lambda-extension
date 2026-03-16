import { invokeLambda, InvocationResult } from './invoke';
import { getDatadogTelemetryByRequestId, DatadogTelemetry } from './datadog';
import { DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../../config';

export interface FunctionConfig {
  functionName: string;
  runtime: string;
}

async function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

/**
 * Worker thread that invokes a Lambda function sequentially
 */
async function invokeThread(
  functionName: string,
  numInvocations: number,
  delayBetweenRequestsMs: number,
  payload: any,
): Promise<InvocationResult[]> {
  const results: InvocationResult[] = [];

  for (let i = 0; i < numInvocations; i++) {
    const result = await invokeLambda(functionName, payload);
    results.push(result);

    // Delay between requests (but not after the last one)
    if (delayBetweenRequestsMs > 0 && i < numInvocations - 1) {
      await sleep(delayBetweenRequestsMs);
    }
  }

  return results;
}

/**
 * Invokes multiple Lambda functions using concurrent threads.
 * Each function gets `concurrency` threads, each doing `invocations` sequential requests.
 *
 * Returns results keyed by runtime, where each value is a list of lists
 * (one per thread, containing telemetry in request order).
 *
 * Example: functions=[{node, fn1}, {python, fn2}], invocations=5, concurrency=2
 *   node:   Thread 0: 5 requests, Thread 1: 5 requests
 *   python: Thread 0: 5 requests, Thread 1: 5 requests
 *   Returns: { node: [[t0], [t1]], python: [[t0], [t1]] }
 */
export async function invokeAndCollectTelemetry(
  functions: FunctionConfig[],
  invocations: number,
  concurrency: number = 1,
  delayBetweenRequestsMs: number = 0,
  payload: any = {},
  datadogIndexingWaitMs: number = DEFAULT_DATADOG_INDEXING_WAIT_MS,
): Promise<Record<string, DatadogTelemetry[][]>> {
  // Start all threads for all functions in parallel
  const allPromises: { runtime: string; functionName: string; promise: Promise<InvocationResult[]> }[] = [];

  for (const fn of functions) {
    for (let t = 0; t < concurrency; t++) {
      allPromises.push({
        runtime: fn.runtime,
        functionName: fn.functionName,
        promise: invokeThread(fn.functionName, invocations, delayBetweenRequestsMs, payload),
      });
    }
  }

  // Wait for all invocations to complete
  const resolvedResults = await Promise.all(
    allPromises.map(async (p) => ({
      runtime: p.runtime,
      functionName: p.functionName,
      results: await p.promise,
    }))
  );

  // Wait for Datadog indexing
  await sleep(datadogIndexingWaitMs);

  // Fetch telemetry and organize by runtime
  const telemetry: Record<string, DatadogTelemetry[][]> = {};

  for (const { runtime, functionName, results } of resolvedResults) {
    if (!telemetry[runtime]) {
      telemetry[runtime] = [];
    }

    const threadTelemetry: DatadogTelemetry[] = [];

    for (const inv of results) {
      try {
        const data = await getDatadogTelemetryByRequestId(functionName, inv.requestId);
        data.statusCode = inv.statusCode;
        threadTelemetry.push(data);
      } catch (err) {
        console.error(`Failed to get Datadog telemetry for requestId ${inv.requestId}:`, err);
        threadTelemetry.push({
          requestId: inv.requestId,
          statusCode: inv.statusCode,
          traces: [],
          logs: [],
        });
      }
    }

    telemetry[runtime].push(threadTelemetry);
  }

  console.log(`Collected telemetry for ${functions.length} functions`);
  return telemetry;
}
