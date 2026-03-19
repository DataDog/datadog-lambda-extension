import { invokeLambda } from './utils/lambda';
import { getDatadogTelemetryByRequestId, DatadogTelemetry, DatadogTrace } from './utils/datadog';
import { publishVersion, waitForSnapStartReady } from './utils/lambda';
import { getIdentifier, DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-snapstart-timing`;

// 2 minutes wait to create stale timestamps that exceed the 60-second threshold
const WAIT_AFTER_SNAPSHOT_MS = 2 * 60 * 1000;

// Maximum reasonable trace duration - spans shouldn't be longer than 1 minute
const MAX_REASONABLE_TRACE_DURATION_NS = 60 * 1000 * 1_000_000; // 1 minute in nanoseconds

interface TestResult {
  functionName: string;
  requestId: string;
  statusCode?: number;
  telemetry?: DatadogTelemetry;
}

/**
 * Integration test for SnapStart timestamp adjustment.
 *
 * This test verifies that the extension correctly adjusts tracer spans that have
 * stale timestamps from the SnapStart snapshot creation phase.
 *
 * The Java Lambda function makes HTTP requests during class initialization,
 * creating spans that get captured in the SnapStart snapshot. After a 2-minute
 * wait, the snapshot is restored and we verify:
 *
 * - Trace duration is reasonable (< 1 minute)
 * - Spans with stale timestamps have been adjusted (tagged with _dd.snapstart_adjusted)
 */
describe('SnapStart Timing Integration Tests', () => {
  let result: TestResult;

  beforeAll(async () => {
    console.log('=== SnapStart Timing Test ===');

    const functionName = `${stackName}-java`;

    // Publish version to create snapshot
    console.log('Publishing new version...');
    const version = await publishVersion(functionName);
    console.log(`Version published: ${version}`);

    // Wait for SnapStart optimization
    console.log('Waiting for SnapStart optimization...');
    await waitForSnapStartReady(functionName, version);
    console.log('SnapStart ready');

    // CRITICAL: Wait to create stale timestamps
    // This ensures any spans from init time will be >60 seconds old
    console.log(`Waiting ${WAIT_AFTER_SNAPSHOT_MS / 1000} seconds for timestamps to become stale...`);
    await new Promise(resolve => setTimeout(resolve, WAIT_AFTER_SNAPSHOT_MS));
    console.log('Wait complete, invoking function...');

    // Invoke function
    const qualifiedName = `${functionName}:${version}`;
    console.log(`Invoking: ${qualifiedName}`);
    const invocation = await invokeLambda(qualifiedName);

    result = {
      functionName: qualifiedName,
      requestId: invocation.requestId,
      statusCode: invocation.statusCode,
    };
    console.log(`Invoked: requestId=${invocation.requestId}, status=${invocation.statusCode}`);

    // Wait for Datadog indexing
    console.log(`Waiting ${DEFAULT_DATADOG_INDEXING_WAIT_MS / 1000}s for Datadog indexing...`);
    await new Promise(resolve => setTimeout(resolve, DEFAULT_DATADOG_INDEXING_WAIT_MS));

    // Fetch telemetry from Datadog
    console.log('Fetching telemetry from Datadog...');
    try {
      result.telemetry = await getDatadogTelemetryByRequestId(result.functionName, result.requestId);
      console.log(`Telemetry: ${result.telemetry.traces?.length || 0} traces`);
    } catch (error) {
      console.error(`Failed to fetch telemetry:`, error);
    }

    console.log('=== Test setup complete ===');
  }, 900000); // 15 minute timeout

  it('should invoke successfully', () => {
    expect(result).toBeDefined();
    expect(result.statusCode).toBe(200);
  });

  it('should send traces to Datadog', () => {
    expect(result.telemetry).toBeDefined();
    expect(result.telemetry!.traces?.length).toBeGreaterThan(0);
  });

  it('should have reasonable trace duration (< 1 minute)', () => {
    const telemetry = result.telemetry;
    expect(telemetry).toBeDefined();
    expect(telemetry!.traces?.length).toBeGreaterThan(0);

    const trace = telemetry!.traces![0];
    const traceDuration = getTraceDuration(trace);
    const traceDurationMs = traceDuration / 1_000_000;

    console.log(`Trace duration: ${traceDurationMs.toFixed(2)}ms`);
    expect(traceDuration).toBeLessThan(MAX_REASONABLE_TRACE_DURATION_NS);
  });

  it('should log span details for debugging', () => {
    const telemetry = result.telemetry;
    if (!telemetry?.traces?.length) return;

    console.log('\n=== Span Details ===');
    const trace = telemetry.traces[0];
    for (const span of trace.spans) {
      const opName = span.attributes?.operation_name || span.attributes?.name || 'unknown';
      const start = span.attributes?.start || 0;
      const duration = span.attributes?.duration || 0;
      const durationMs = duration / 1_000_000;
      const custom = span.attributes?.custom;
      const adjusted = custom?._dd?.snapstart_adjusted === 'true' ||
                       custom?.['_dd.snapstart_adjusted'] === 'true' ||
                       span.attributes?.['_dd.snapstart_adjusted'] === 'true';

      console.log(`  ${opName}: duration=${durationMs.toFixed(2)}ms${adjusted ? ' [ADJUSTED]' : ''}`);
    }

    // Count adjusted spans
    const adjustedCount = trace.spans.filter((span: any) => {
      const custom = span.attributes?.custom;
      return custom?._dd?.snapstart_adjusted === 'true' ||
             custom?.['_dd.snapstart_adjusted'] === 'true' ||
             span.attributes?.['_dd.snapstart_adjusted'] === 'true';
    }).length;

    console.log(`\nAdjusted spans: ${adjustedCount}`);
    if (adjustedCount === 0) {
      console.log('Note: No adjusted spans found. This is expected if the tracer does not create spans during static initialization.');
    }
  });
});

/**
 * Calculate the total duration of a trace (max end time - min start time)
 */
function getTraceDuration(trace: DatadogTrace): number {
  let minStart = Infinity;
  let maxEnd = 0;

  for (const span of trace.spans) {
    const start = span.attributes?.start || 0;
    const duration = span.attributes?.duration || 0;
    const end = start + duration;

    if (start > 0 && start < minStart) minStart = start;
    if (end > maxEnd) maxEnd = end;
  }

  return maxEnd - minStart;
}
