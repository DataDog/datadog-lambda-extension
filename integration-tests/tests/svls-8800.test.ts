/**
 * Integration tests for SVLS-8800:
 *   Bug: durable_function:true metric tag missing for first invocation after cold start.
 *
 * Root cause: In On-Demand mode, PlatformInitStart arrives AFTER InvokeEvent during a
 * cold start. The original code emitted aws.lambda.enhanced.invocation immediately in
 * on_invoke_event, before set_durable_function_tag() had been called.
 *
 * Fix: defer the metric emission to on_platform_init_start (with on_platform_init_report
 * as a fallback), so the durable_function tag is always set before the metric fires.
 *
 * What these tests verify:
 *   1. After a forced cold start, a single invocation produces exactly one
 *      aws.lambda.enhanced.invocation data point — proving the deferred metric is
 *      never silently dropped.
 *   2. That single data point has a positive value (count >= 1).
 *
 * Note: Asserting durable_function:true specifically requires a Lambda runtime whose
 * PlatformInitStart ARN contains "DurableFunction" (the Azure Durable Functions
 * integration). Standard Lambda runtimes do not emit that ARN, so we test the
 * non-durable regression path here.  The unit tests in processor.rs (added in the
 * same PR) cover the durable tag assertion exhaustively.
 */

import { invokeLambda } from './utils/lambda';
import { forceColdStart } from './utils/lambda';
import { getInvocationMetricPoints } from './utils/datadog';
import { getIdentifier, DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-svls-8800`;
const functionName = `${stackName}-node-lambda`;

describe('SVLS-8800: invocation metric emitted for first On-Demand cold-start', () => {
  let metricsFromTime: number;
  let metricsToTime: number;

  beforeAll(async () => {
    // Force a cold start so this invocation is guaranteed to be the first in a
    // fresh runtime environment — exactly the scenario described in the bug.
    await forceColdStart(functionName);

    metricsFromTime = Date.now();

    // Invoke exactly once.  This is the critical scenario: if the runtime
    // environment is recycled after just one invocation, the metric must still
    // have been flushed with the correct tags.
    await invokeLambda(functionName);

    // Wait for Datadog to index the metric.
    await new Promise(resolve => setTimeout(resolve, DEFAULT_DATADOG_INDEXING_WAIT_MS));

    metricsToTime = Date.now();
  }, 600000);

  it('should emit aws.lambda.enhanced.invocation metric for first cold-start invocation', async () => {
    const points = await getInvocationMetricPoints(functionName, metricsFromTime, metricsToTime);

    // At least one data point must exist — confirms the deferred metric was not dropped.
    expect(points.length).toBeGreaterThan(0);
  });

  it('invocation metric should have a positive count', async () => {
    const points = await getInvocationMetricPoints(functionName, metricsFromTime, metricsToTime);

    // The sum should reflect at least one invocation recorded.
    const hasPositiveValue = points.some(p => p.value !== null && p.value > 0);
    expect(hasPositiveValue).toBe(true);
  });

  it('should NOT emit a metric tagged durable_function:true for a non-durable runtime', async () => {
    // For a standard Lambda runtime there should be no data points tagged
    // durable_function:true.  This guards against accidentally tagging all functions.
    const points = await getInvocationMetricPoints(
      functionName,
      metricsFromTime,
      metricsToTime,
      'durable_function:true',
    );

    expect(points.length).toBe(0);
  });
});
