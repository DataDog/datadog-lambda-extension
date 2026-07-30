import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { forceColdStart } from './utils/lambda';
import { filterLogMessages } from './utils/cloudwatch';
import { IDENTIFIER } from '../config';

// The enriched payload must be large enough to need a high batch cap, yet stay
// under the 12 MB cap so it flushes in a single batch without a 413.
const MIN_ENRICHED_BYTES = 10_000_000;

// Trace config, sent in the invocation payload. 400 x 24 KB enriches to ~10 MB.
const SPAN_COUNT = 400;
const PAYLOAD_BYTES = 24_000;

const stackName = `${IDENTIFIER}-payload-size`;

describe('Payload Size Integration Tests', () => {

  describe('large single-invocation trace', () => {
    let telemetry: Record<string, DatadogTelemetry>;
    let enrichedPayloadBytes: number | undefined;
    let batchedPayloadBytes: number | undefined;
    let sendErrorMessages: string[] = [];

    const functionName = `${stackName}-large-trace-lambda`;

    beforeAll(async () => {
      const functions: FunctionConfig[] = [
        { functionName, runtime: 'node' },
      ];

      await Promise.all(functions.map(fn => forceColdStart(fn.functionName)));

      const startTime = Date.now() - 60_000;

      // Invoke a few times. A cold invocation delivers its large (~10 MB) trace
      // to the extension too late to make that invocation's end-of-invocation
      // flush, so it flushes on a following invocation. The extra invocations
      // give the first request's trace a flush to ride out on.
      telemetry = await invokeAndCollectTelemetry(
        functions, 3, 1, 2000, { spanCount: SPAN_COUNT, payloadBytes: PAYLOAD_BYTES });

      const enrichedMessages = await filterLogMessages(
        functionName,
        '"payload size after enrichment"',
        startTime,
        Date.now(),
      );
      enrichedPayloadBytes = getMaxLoggedBytes(enrichedMessages, /payload size after enrichment: (\d+) bytes/);
      console.log(`Extension reported enriched payload size: ${enrichedPayloadBytes} bytes`);

      const batchedMessages = await filterLogMessages(
        functionName,
        '"totaling"',
        startTime,
        Date.now(),
      );
      batchedPayloadBytes = getMaxLoggedBytes(batchedMessages, /totaling (\d+) bytes/);
      console.log(`Extension reported batched payload size: ${batchedPayloadBytes} bytes`);

      // A payload over the intake limit logs "Max retries exceeded, returning
      // HTTP error" with status=413. Capture any such lines so we can assert the
      // extension flushed without a 413.
      sendErrorMessages = await filterLogMessages(
        functionName,
        '?"Max retries exceeded" ?"status=413" ?"Payload Too Large"',
        startTime,
        Date.now(),
      );
      console.log(`Extension send-error log lines: ${sendErrorMessages.length}`);

      console.log('Invocation and telemetry collection complete');
    }, 1800000);

    // Assert on the FIRST request's trace. Its flush is deferred to a later
    // invocation (cold-start race), which is why we invoke a few times — but the
    // trace is tagged with the first request's id, so it's found here.
    const getInvocation = () => telemetry.node?.threads[0]?.[0];

    it('should invoke Lambda successfully', () => {
      const result = getInvocation();
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    // Guards that the trace is actually large enough to exercise the high cap.
    it('should have a large enriched payload', () => {
      expect(enrichedPayloadBytes).toBeDefined();
      expect(enrichedPayloadBytes!).toBeGreaterThan(MIN_ENRICHED_BYTES);
    });

    it('should have a large batched payload', () => {
      expect(batchedPayloadBytes).toBeDefined();
      expect(batchedPayloadBytes!).toBeGreaterThan(MIN_ENRICHED_BYTES);
    });

    it('should flush without a 413 Payload Too Large error', () => {
      expect(sendErrorMessages).toEqual([]);
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

function getMaxLoggedBytes(messages: string[], pattern: RegExp): number | undefined {
  let max: number | undefined;
  for (const message of messages) {
    const match = message.match(pattern);
    if (match) {
      const bytes = Number(match[1]);
      if (max === undefined || bytes > max) {
        max = bytes;
      }
    }
  }
  return max;
}