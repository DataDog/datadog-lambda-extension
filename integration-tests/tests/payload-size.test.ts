import { invokeLambda, forceColdStart } from './utils/lambda';
import { filterLogMessages, countLogEvents } from './utils/cloudwatch';
import {
  SPAN_COUNT,
  PAYLOAD_BYTES,
  INVOCATION_COUNT,
  DELAY_BETWEEN_INVOCATIONS_MS,
  sleep,
} from './utils/payload-size';
import { IDENTIFIER } from '../config';

// The enriched payload must be large enough to need a high batch cap, yet stay
// under the 12 MB cap so it flushes in a single batch without a 413.
const MIN_ENRICHED_BYTES = 10_000_000;

// CloudWatch Logs searchability lags ingestion; give the extension's debug
// lines up to this long to become searchable.
const LOG_SEARCHABLE_TIMEOUT_MS = 5 * 60 * 1000;
const LOG_POLL_INTERVAL_MS = 10_000;

// These terminal outcomes are logged only after the trace send and any retries
// complete. Wait for one before treating the absence of a 413 as meaningful.
const TRACE_SEND_COMPLETION_FILTER =
  '?"TRACES | Successfully sent trace" ?"TRACES | Request failed after"';

const stackName = `${IDENTIFIER}-payload-size`;

describe('Payload Size Integration Tests', () => {

  describe('large single-invocation trace', () => {
    let invocationStatusCodes: (number | undefined)[] = [];
    let enrichedPayloadBytes: number | undefined;
    let batchedPayloadBytes: number | undefined;
    let traceSendCompletionMessages: string[] = [];
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
          await sleep(DELAY_BETWEEN_INVOCATIONS_MS);
        }
      }

      // CloudWatch Logs is eventually consistent: FilterLogEvents can return
      // nothing for events written seconds earlier. Poll until the extension's
      // debug lines become searchable instead of querying once immediately
      // after the invocations.
      [enrichedPayloadBytes, batchedPayloadBytes] = await Promise.all([
        pollForMaxLoggedBytes(
          functionName,
          startTime,
          '"payload size after enrichment"',
          /payload size after enrichment: (\d+) bytes/,
          'enriched',
        ),
        pollForMaxLoggedBytes(
          functionName,
          startTime,
          '"totaling"',
          /totaling (\d+) bytes/,
          'batched',
        ),
      ]);

      // A payload over the intake limit logs "Max retries exceeded, returning
      // HTTP error" with status=413. First wait for the terminal success or
      // failure log emitted after trace.send completes, then capture any 413
      // lines. This makes an empty result meaningful rather than an indexing
      // race with an in-flight send.
      traceSendCompletionMessages = await pollForLogMessages(
        functionName,
        startTime,
        TRACE_SEND_COMPLETION_FILTER,
        'trace send completion',
      );
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
      expect(invocationStatusCodes).toEqual(Array(INVOCATION_COUNT).fill(200));
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

    it('should complete a trace send', () => {
      expect(traceSendCompletionMessages.length).toBeGreaterThan(0);
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

async function pollForLogMessages(
  functionName: string,
  startTime: number,
  filterPattern: string,
  label: string,
): Promise<string[]> {
  const deadline = Date.now() + LOG_SEARCHABLE_TIMEOUT_MS;
  let attempt = 0;
  while (Date.now() < deadline) {
    attempt += 1;
    const messages = await filterLogMessages(functionName, filterPattern, startTime, Date.now());
    if (messages.length > 0) {
      console.log(`Found ${label} log lines: ${messages.length} (attempt ${attempt})`);
      return messages;
    }
    await sleep(LOG_POLL_INTERVAL_MS);
  }
  console.log(
    `Timed out after ${LOG_SEARCHABLE_TIMEOUT_MS / 1000}s waiting for ${label} log lines (${attempt} attempts)`,
  );
  return [];
}

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

/**
 * Polls the function's CloudWatch logs until a message matching `pattern` is
 * found, returning the maximum captured value, or undefined on timeout.
 */
async function pollForMaxLoggedBytes(
  functionName: string,
  startTime: number,
  filterPattern: string,
  pattern: RegExp,
  label: string,
): Promise<number | undefined> {
  const deadline = Date.now() + LOG_SEARCHABLE_TIMEOUT_MS;
  let attempt = 0;
  while (Date.now() < deadline) {
    attempt += 1;
    const messages = await filterLogMessages(functionName, filterPattern, startTime, Date.now());
    const max = getMaxLoggedBytes(messages, pattern);
    if (max !== undefined) {
      console.log(`Extension reported ${label} payload size: ${max} bytes (attempt ${attempt})`);
      return max;
    }
    await sleep(LOG_POLL_INTERVAL_MS);
  }
  console.log(
    `Timed out after ${LOG_SEARCHABLE_TIMEOUT_MS / 1000}s waiting for "${filterPattern}" log lines (${attempt} attempts)`,
  );
  // Distinguish "the extension never logged anything" from "the extension
  // logged but these lines are missing or not yet searchable".
  const totalEvents = await countLogEvents(functionName, startTime, Date.now());
  const traceLines = await filterLogMessages(
    functionName,
    '"TRACES"',
    startTime,
    Date.now(),
    20,
  );
  console.log(
    `Diagnostics: ${totalEvents} log events in window, ${traceLines.length} extension "TRACES" lines`,
  );
  if (traceLines.length > 0) {
    const last = traceLines[traceLines.length - 1];
    console.log(`Last TRACES line: ${last.length > 300 ? `${last.slice(0, 300)}...` : last}`);
  }
  return undefined;
}
