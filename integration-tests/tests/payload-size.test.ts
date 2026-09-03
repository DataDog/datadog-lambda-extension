import { invokeLambda, forceColdStart } from './utils/lambda';
import { filterLogMessages } from './utils/cloudwatch';
import {
  SPAN_COUNT,
  PAYLOAD_BYTES,
  INVOCATION_COUNT,
  DELAY_BETWEEN_INVOCATIONS_MS,
} from './utils/payload-size';
import { IDENTIFIER } from '../config';

// The enriched payload must be large enough to need a high batch cap, yet stay
// under the 12 MB cap so it flushes in a single batch without a 413.
const MIN_ENRICHED_BYTES = 10_000_000;

const stackName = `${IDENTIFIER}-payload-size`;

describe('Payload Size Integration Tests', () => {

  describe('large single-invocation trace', () => {
    let invocationStatusCodes: (number | undefined)[] = [];
    let enrichedPayloadBytes: number | undefined;
    let batchedPayloadBytes: number | undefined;
    let sendErrorMessages: string[] = [];

    const functionName = `${stackName}-large-trace-lambda`;

    beforeAll(async () => {
      await forceColdStart(functionName);

      const startTime = Date.now() - 60_000;

      // Invoke a few times so the first request's large trace gets a flush to
      // ride out on (cold-start race). Only extension-side behavior is checked
      // here: payload sizes and the absence of 413s, read from the logs.
      invocationStatusCodes = [];
      for (let i = 0; i < INVOCATION_COUNT; i++) {
        const result = await invokeLambda(
          functionName, { spanCount: SPAN_COUNT, payloadBytes: PAYLOAD_BYTES });
        invocationStatusCodes.push(result.statusCode);
        if (i < INVOCATION_COUNT - 1) {
          await new Promise(resolve => setTimeout(resolve, DELAY_BETWEEN_INVOCATIONS_MS));
        }
      }

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

    it('should invoke Lambda successfully', () => {
      expect(invocationStatusCodes.length).toBe(INVOCATION_COUNT);
      expect(invocationStatusCodes[0]).toBe(200);
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

    // Backend delivery (exactly one trace, root span, all spans) is asserted in
    // the payload-size-e2e suite, which is allowed to fail: large traces are
    // intermittently dropped downstream after a successful send, which is a
    // backend issue outside the extension's control.
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
