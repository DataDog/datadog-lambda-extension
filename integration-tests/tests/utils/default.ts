import { invokeLambda, InvocationResult } from './lambda';
import {
  getInvocationTracesLogsByRequestId,
  InvocationTracesLogs,
  DatadogTelemetry,
  getEnhancedMetrics,
} from './datadog';
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

    if (delayBetweenRequestsMs > 0 && i < numInvocations - 1) {
      await sleep(delayBetweenRequestsMs);
    }
  }

  return results;
}

/**
 * Invokes multiple Lambda functions and collects all telemetry (traces, logs, metrics).
 * Returns DatadogTelemetry per runtime, which includes per-invocation data and aggregated metrics.
 *
 * Example: functions=[{node, fn1}, {python, fn2}], invocations=2
 *   Returns: {
 *     node: { invocations: [inv1, inv2], metrics: { duration: {...} } },
 *     python: { invocations: [inv1, inv2], metrics: { duration: {...} } }
 *   }
 */
export async function invokeAndCollectTelemetry(
  functions: FunctionConfig[],
  invocations: number,
  concurrency: number = 1,
  delayBetweenRequestsMs: number = 0,
  payload: any = {},
  datadogIndexingWaitMs: number = DEFAULT_DATADOG_INDEXING_WAIT_MS,
): Promise<Record<string, DatadogTelemetry>> {
  const invocationStartTime = Date.now();

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

  const resolvedResults = await Promise.all(
    allPromises.map(async (p) => ({
      runtime: p.runtime,
      functionName: p.functionName,
      results: await p.promise,
    }))
  );

  await sleep(datadogIndexingWaitMs);

  const metricsEndTime = Date.now();

  const telemetryByRuntime: Record<string, InvocationTracesLogs[][]> = {};

  for (const { runtime, functionName, results } of resolvedResults) {
    if (!telemetryByRuntime[runtime]) {
      telemetryByRuntime[runtime] = [];
    }

    const threadTelemetry: InvocationTracesLogs[] = [];

    for (const inv of results) {
      try {
        const data = await getInvocationTracesLogsByRequestId(functionName, inv.requestId);
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

    telemetryByRuntime[runtime].push(threadTelemetry);
  }

  const runtimesWithFunctions = functions.map(fn => ({
    runtime: fn.runtime,
    functionName: fn.functionName,
  }));

  const metricsPromises = runtimesWithFunctions.map(async ({ runtime, functionName }) => {
    const metrics = await getEnhancedMetrics(functionName, invocationStartTime, metricsEndTime);
    return { runtime, metrics };
  });

  const metricsResults = await Promise.all(metricsPromises);

  const result: Record<string, DatadogTelemetry> = {};

  for (const fn of functions) {
    const threads = telemetryByRuntime[fn.runtime] || [];
    const metricsResult = metricsResults.find(m => m.runtime === fn.runtime)!;
    result[fn.runtime] = {
      threads,
      metrics: metricsResult.metrics,
    };
  }

  console.log(`Collected telemetry for ${functions.length} functions`);
  return result;
}
