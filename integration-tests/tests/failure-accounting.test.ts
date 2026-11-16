import { TooManyRequestsException } from '@aws-sdk/client-lambda';
import { invokeLambda, InvocationResult } from './utils/lambda';
import { getMetricCount } from './utils/datadog';
import { IDENTIFIER } from '../config';

/**
 * Failure-accounting test: every timeout / crash must be reflected in
 * `aws.lambda.enhanced.errors` and `aws.lambda.enhanced.timeouts`.
 *
 * The extension can lose these metrics when an invocation fails after it has
 * switched from End-of-invocation flushing to periodic/continuous flushing.
 * The Default strategy only adapts after ~20 invocations in one execution
 * environment, and a timeout/crash resets the environment (new extension
 * process, back in End mode). Failures are therefore spaced 30 invocations
 * apart so each fresh environment has adapted before the next failure.
 * Reserved concurrency 1 plus sequential invokes keep everything in one
 * environment at a time.
 *
 * The `end`-strategy function never adapts, so it is the control: if it also
 * miscounts, the problem is in the test or ingestion rather than the
 * flush-mode transition.
 */
const stackName = `${IDENTIFIER}-failure-accounting`;

interface FailureCase {
  strategy: string;
  functionName: string;
}

const cases: FailureCase[] = [
  { strategy: 'default', functionName: `${stackName}-default-lambda` },
  { strategy: 'end',     functionName: `${stackName}-end-lambda` },
];

type Mode = 'ok' | 'timeout' | 'crash';

const FAILURE_SPACING = 30;
// The loss is a race, so run enough resets to hit it reliably.
const FAILURE_COUNT = 12;
// Trailing successes so the environment created after the last failure also runs.
const TOTAL_INVOCATIONS = FAILURE_SPACING * FAILURE_COUNT + 10;

const INVOCATIONS_METRIC = 'aws.lambda.enhanced.invocations';
const ERRORS_METRIC = 'aws.lambda.enhanced.errors';
const TIMEOUTS_METRIC = 'aws.lambda.enhanced.timeouts';

const INITIAL_WAIT_MS = 90 * 1000;   // wait before first query
const POLL_INTERVAL_MS = 30 * 1000;  // re-query cadence
const TOTAL_BUDGET_MS = 12 * 60 * 1000; // polling ceiling, measured from the last invoke
const INVOKE_PHASE_BUDGET_MS = 15 * 60 * 1000;

// Failure k (1-based) lands on invocation 30k; odd k times out, even k crashes.
function modeFor(invocation: number): Mode {
  if (invocation % FAILURE_SPACING !== 0 || invocation / FAILURE_SPACING > FAILURE_COUNT) {
    return 'ok';
  }
  return (invocation / FAILURE_SPACING) % 2 === 1 ? 'timeout' : 'crash';
}

interface Counts {
  invocations: number;
  errors: number;
  timeouts: number;
}

function isTimeout(result: InvocationResult): boolean {
  try {
    const body = JSON.parse(result.payload ?? '{}');
    return body.errorType === 'Sandbox.Timedout' || /timed out/i.test(body.errorMessage ?? '');
  } catch {
    return false;
  }
}

async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// With reserved concurrency 1, the invoke right after a timeout/crash can be
// throttled while the old environment shuts down. Throttled invokes never run,
// so retrying does not affect the ground truth.
async function invokeWithThrottleRetry(functionName: string, payload: object): Promise<InvocationResult> {
  for (let attempt = 1; ; attempt++) {
    try {
      return await invokeLambda(functionName, payload);
    } catch (err) {
      if (!(err instanceof TooManyRequestsException) || attempt >= 10) {
        throw err;
      }
      await sleep(1000 * attempt);
    }
  }
}

async function runSchedule(functionName: string): Promise<Counts> {
  const truth: Counts = { invocations: 0, errors: 0, timeouts: 0 };
  for (let i = 1; i <= TOTAL_INVOCATIONS; i++) {
    const mode = modeFor(i);
    const result = await invokeWithThrottleRetry(functionName, { mode });
    truth.invocations++;
    if (result.functionError) {
      truth.errors++;
      if (isTimeout(result)) {
        truth.timeouts++;
      }
      console.log(`${functionName} #${i} mode=${mode} functionError=${result.functionError} payload=${result.payload}`);
    } else if (mode !== 'ok') {
      console.warn(`${functionName} #${i} mode=${mode} unexpectedly succeeded`);
    }
  }
  return truth;
}

async function fetchCounts(functionName: string, start: number, end: number): Promise<Counts> {
  const [invocations, errors, timeouts] = await Promise.all([
    getMetricCount(INVOCATIONS_METRIC, functionName, start, end),
    getMetricCount(ERRORS_METRIC, functionName, start, end),
    getMetricCount(TIMEOUTS_METRIC, functionName, start, end),
  ]);
  return { invocations, errors, timeouts };
}

function reachedExpected(observed: Counts, expected: Counts): boolean {
  return observed.invocations >= expected.invocations
    && observed.errors >= expected.errors
    && observed.timeouts >= expected.timeouts;
}

describe('Failure Accounting Integration Tests', () => {
  const truthByStrategy: Record<string, Counts> = {};
  const observedByStrategy: Record<string, Counts> = {};

  beforeAll(async () => {
    // Same bucket-alignment margin as oom.test.ts.
    const windowStart = Date.now() - 60 * 1000;

    const truths = await Promise.all(cases.map((c) => runSchedule(c.functionName)));
    cases.forEach((c, i) => { truthByStrategy[c.strategy] = truths[i]; });
    console.log('Ground truth by strategy:', truthByStrategy);

    const invokesDone = Date.now();
    await sleep(INITIAL_WAIT_MS);

    const deadline = invokesDone + TOTAL_BUDGET_MS;
    let attempt = 0;
    while (Date.now() < deadline) {
      attempt++;
      const now = Date.now();
      const observed = await Promise.all(cases.map((c) => fetchCounts(c.functionName, windowStart, now)));
      cases.forEach((c, i) => { observedByStrategy[c.strategy] = observed[i]; });
      const pending = cases.filter((c) => !reachedExpected(observedByStrategy[c.strategy], truthByStrategy[c.strategy]));
      console.log(`Failure-accounting poll #${attempt}:`, observedByStrategy, pending.length ? `(pending: ${pending.map((c) => c.strategy).join(', ')})` : '(all reached expected)');
      if (pending.length === 0) {
        break;
      }
      await sleep(POLL_INTERVAL_MS);
    }

    console.log('Observed counts by strategy (final):', observedByStrategy);
  }, INVOKE_PHASE_BUDGET_MS + TOTAL_BUDGET_MS + 60 * 1000);

  describe.each(cases)('$strategy flush strategy', ({ strategy }) => {
    it('should produce the scheduled failures (test harness sanity check)', () => {
      const truth = truthByStrategy[strategy];
      expect(truth.invocations).toBe(TOTAL_INVOCATIONS);
      expect(truth.errors).toBe(FAILURE_COUNT);
      expect(truth.timeouts).toBe(FAILURE_COUNT / 2);
    });

    it('should report one invocations metric per invocation', () => {
      expect(observedByStrategy[strategy]?.invocations).toBe(truthByStrategy[strategy].invocations);
    });

    it('should report one errors metric per timeout or crash', () => {
      expect(observedByStrategy[strategy]?.errors).toBe(truthByStrategy[strategy].errors);
    });

    it('should report one timeouts metric per timeout', () => {
      expect(observedByStrategy[strategy]?.timeouts).toBe(truthByStrategy[strategy].timeouts);
    });
  });
});
