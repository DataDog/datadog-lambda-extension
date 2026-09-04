import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry, getInvocationTracesLogsByRequestId, InvocationTracesLogs } from './utils/datadog';
import { forceColdStart } from './utils/lambda';
import {
  SPAN_COUNT,
  PAYLOAD_BYTES,
  INVOCATION_COUNT,
  DELAY_BETWEEN_INVOCATIONS_MS,
  sleep,
} from './utils/payload-size';
import { IDENTIFIER } from '../config';

// Indexing of the ~10 MB trace can lag well past the default 5-minute wait in
// invokeAndCollectTelemetry, and it lands progressively: the root span becomes
// searchable long before the last of the SPAN_COUNT payload spans. The poll
// below therefore waits for the trace to be COMPLETE, not merely present.
const TRACE_INDEXING_TIMEOUT_MS = 10 * 60 * 1000;
const TRACE_INDEXING_POLL_INTERVAL_MS = 30 * 1000;

const stackName = `${IDENTIFIER}-payload-size-e2e`;

/**
 * Backend-delivery checks for the large single-invocation trace. This suite is
 * `allow_failure: true` in CI: the extension reliably sends the ~10 MB payload
 * without a 413 (asserted in the blocking payload-size suite), but the backend
 * intermittently drops or truncates the trace after intake. Failures here are
 * kept visible as evidence for that backend issue, not treated as regressions.
 */
describe('Payload Size E2E Delivery Tests', () => {

  describe('large single-invocation trace', () => {
    let telemetry: Record<string, DatadogTelemetry>;

    const functionName = `${stackName}-large-trace-lambda`;

    beforeAll(async () => {
      const functions: FunctionConfig[] = [
        { functionName, runtime: 'node' },
      ];

      await Promise.all(functions.map(fn => forceColdStart(fn.functionName)));

      telemetry = await invokeAndCollectTelemetry(
        functions, INVOCATION_COUNT, 1, DELAY_BETWEEN_INVOCATIONS_MS,
        { spanCount: SPAN_COUNT, payloadBytes: PAYLOAD_BYTES });

      // The assertions below target the FIRST request's trace. Its ~10 MB of
      // spans can take longer than the default indexing wait to become fully
      // searchable, so poll for the complete trace before the assertions run.
      const firstInvocation = telemetry.node?.threads[0]?.[0];
      if (firstInvocation) {
        telemetry.node.threads[0][0] = await waitForCompleteTrace(functionName, firstInvocation);
      }

      console.log('Invocation and telemetry collection complete');
    }, 1800000);

    // Assert on the FIRST request's trace. Its flush is deferred to a later
    // invocation (cold-start race), which is why we invoke a few times, but the
    // trace is tagged with the first request's id, so it's found here. The
    // beforeAll hook polls for it if indexing lags past the default wait.
    const getInvocation = () => telemetry.node?.threads[0]?.[0];

    it('should invoke Lambda successfully', () => {
      const result = getInvocation();
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should deliver exactly one trace to Datadog', () => {
      const result = getInvocation();
      expect(result).toBeDefined();
      expect(result.traces?.length).toBe(1);
    });

    it('should have the aws.lambda root span', () => {
      const result = getInvocation();
      expect(result).toBeDefined();

      const allSpans = result.traces!.flatMap(t => t.spans);
      const awsLambdaSpan = allSpans.find(
        (span: any) => span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();
    });

    it('should contain all the payload-carrying spans from the large trace', () => {
      // Exactly the SPAN_COUNT order.process spans we emitted should come back
      // (SPAN_COUNT < the 1000-span API page limit, so none are truncated).
      const result = getInvocation();
      expect(result).toBeDefined();

      const orderSpans = result
        .traces!.flatMap(t => t.spans)
        .filter((span: any) => span.attributes.operation_name === 'order.process');
      expect(orderSpans.length).toBe(SPAN_COUNT);
    });
  });
});

/**
 * Number of payload-carrying spans currently indexed for an invocation. Used as
 * the poll's completeness signal: `traces.length > 0` goes true as soon as the
 * root span is indexed, which is far earlier than the point the assertions
 * below need (all SPAN_COUNT `order.process` spans searchable).
 */
function countOrderSpans(invocation: InvocationTracesLogs): number {
  return (invocation.traces ?? [])
    .flatMap(t => t.spans)
    .filter((span: any) => span.attributes?.operation_name === 'order.process')
    .length;
}

/**
 * Polls until the invocation's trace is fully indexed in Datadog (all
 * SPAN_COUNT payload spans searchable) or the timeout elapses. Returns the most
 * complete result seen, so assertions fail with real data when it never
 * completes.
 */
async function waitForCompleteTrace(
  functionName: string,
  invocation: InvocationTracesLogs,
): Promise<InvocationTracesLogs> {
  let best = invocation;
  let bestCount = countOrderSpans(invocation);
  if (bestCount >= SPAN_COUNT) {
    return best;
  }

  const deadline = Date.now() + TRACE_INDEXING_TIMEOUT_MS;
  let attempt = 0;
  while (Date.now() < deadline) {
    attempt += 1;
    console.log(
      `Trace for ${invocation.requestId} has ${bestCount}/${SPAN_COUNT} order.process spans ` +
      `(attempt ${attempt}), retrying in ${TRACE_INDEXING_POLL_INTERVAL_MS / 1000}s...`);
    await sleep(TRACE_INDEXING_POLL_INTERVAL_MS);

    let latest: InvocationTracesLogs;
    try {
      latest = await getInvocationTracesLogsByRequestId(functionName, invocation.requestId);
    } catch (err) {
      console.error(`Failed to query traces for ${invocation.requestId}:`, err);
      continue;
    }
    latest.statusCode = invocation.statusCode;

    const count = countOrderSpans(latest);
    if (count >= bestCount) {
      best = latest;
      bestCount = count;
    }
    if (bestCount >= SPAN_COUNT) {
      console.log(`Complete trace indexed for ${invocation.requestId} after ${attempt} poll(s)`);
      return best;
    }
  }

  console.warn(
    `Trace for ${invocation.requestId} still incomplete ` +
    `(${bestCount}/${SPAN_COUNT} order.process spans) after ${TRACE_INDEXING_TIMEOUT_MS / 1000}s`);
  return best;
}
