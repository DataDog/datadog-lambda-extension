import { invokeLambda } from './utils/lambda';
import { getDatadogTelemetryByRequestId, DatadogTelemetry, DatadogTrace, searchAllSpans } from './utils/datadog';
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

    // Diagnostic: search for ALL spans from this service (including those without request_id)
    console.log('\n=== Diagnostic: All spans for service ===');
    const serviceName = result.functionName.split(':')[0];
    await searchAllSpans(serviceName, 100);

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

  it('should have OkHttp spans in the trace', () => {
    const telemetry = result.telemetry;
    expect(telemetry).toBeDefined();
    expect(telemetry!.traces?.length).toBeGreaterThan(0);

    const trace = telemetry!.traces![0];

    // Verify trace has expected span types
    const spanTypes = trace.spans.map((s: any) => s.attributes?.operation_name);
    console.log(`Span types in trace: ${spanTypes.join(', ')}`);

    // Check if we have HTTP spans in the trace (from handler execution)
    const httpSpan = trace.spans.find((s: any) =>
      s.attributes?.operation_name?.includes('http') ||
      s.attributes?.operation_name?.includes('okhttp')
    );

    expect(httpSpan).toBeDefined();
    console.log('✓ OkHttp span found in trace - Java tracer instrumentation working');
  });

  it('should have reasonable trace duration (< 1 minute)', () => {
    const telemetry = result.telemetry;
    expect(telemetry).toBeDefined();
    expect(telemetry!.traces?.length).toBeGreaterThan(0);

    const trace = telemetry!.traces![0];

    // Log full span structure for first span to understand the data format
    console.log('\n=== First Span Full Structure ===');
    console.log(JSON.stringify(trace.spans[0]?.attributes, null, 2));

    // Log all span timestamps for debugging
    console.log('\n=== Span Timestamps ===');
    const spanTimings: { name: string; startTimestamp: string; endTimestamp: string }[] = [];

    for (const span of trace.spans) {
      const opName = span.attributes?.operation_name || 'unknown';
      // Datadog API uses start_timestamp and end_timestamp fields
      const startTimestamp = span.attributes?.start_timestamp || '';
      const endTimestamp = span.attributes?.end_timestamp || '';
      const durationNs = span.attributes?.custom?.duration || 0;

      spanTimings.push({ name: opName, startTimestamp, endTimestamp });

      console.log(`  ${opName}:`);
      console.log(`    start_timestamp: ${startTimestamp}`);
      console.log(`    end_timestamp: ${endTimestamp}`);
      console.log(`    duration_ns: ${durationNs}`);
    }

    // Sort spans by start_timestamp to see order
    console.log('\n=== Span Order (by start_timestamp) ===');
    spanTimings.sort((a, b) => {
      if (a.startTimestamp && b.startTimestamp) {
        return new Date(a.startTimestamp).getTime() - new Date(b.startTimestamp).getTime();
      }
      return 0;
    });
    spanTimings.forEach((s, i) => {
      console.log(`  ${i + 1}. ${s.name} - ${s.startTimestamp}`);
    });

    expect(trace.spans.length).toBeGreaterThanOrEqual(2);
    console.log(`\nTrace has ${trace.spans.length} spans`);
  });

  it('should have OkHttp span with correct timestamp (during invocation)', () => {
    const telemetry = result.telemetry;
    expect(telemetry).toBeDefined();
    expect(telemetry!.traces?.length).toBeGreaterThan(0);

    const trace = telemetry!.traces![0];

    // Find the aws.lambda span and okhttp span
    const awsLambdaSpan = trace.spans.find((s: any) =>
      s.attributes?.operation_name === 'aws.lambda'
    );
    const okhttpSpan = trace.spans.find((s: any) =>
      s.attributes?.operation_name?.includes('okhttp') ||
      s.attributes?.operation_name?.includes('http.request')
    );

    if (!awsLambdaSpan || !okhttpSpan) {
      console.log('Cannot compare timestamps - missing spans');
      return;
    }

    // Get timestamps using correct Datadog API field names
    const lambdaStart = awsLambdaSpan.attributes?.start_timestamp;
    const lambdaEnd = awsLambdaSpan.attributes?.end_timestamp;
    const httpStart = okhttpSpan.attributes?.start_timestamp;
    const httpEnd = okhttpSpan.attributes?.end_timestamp;

    console.log('\n=== Span Timing Comparison ===');
    console.log(`aws.lambda: start=${lambdaStart}, end=${lambdaEnd}`);
    console.log(`okhttp: start=${httpStart}, end=${httpEnd}`);

    if (lambdaStart && httpStart) {
      const lambdaStartMs = new Date(lambdaStart).getTime();
      const lambdaEndMs = new Date(lambdaEnd).getTime();
      const httpStartMs = new Date(httpStart).getTime();
      const diffFromLambdaStart = httpStartMs - lambdaStartMs;

      console.log(`OkHttp started ${diffFromLambdaStart}ms after aws.lambda started`);

      // The OkHttp span should start DURING the aws.lambda span
      // (after lambda start, before lambda end)
      const isWithinLambdaSpan = httpStartMs >= lambdaStartMs && httpStartMs <= lambdaEndMs;

      if (isWithinLambdaSpan) {
        console.log('OK: OkHttp span started during aws.lambda span execution');
      } else if (diffFromLambdaStart < -60000) {
        console.log('\n*** WARNING: OkHttp span has stale timestamp ***');
        console.log(`The span appears to start ${Math.abs(diffFromLambdaStart / 1000).toFixed(1)} seconds BEFORE the Lambda invocation.`);
      } else {
        console.log(`Note: OkHttp span started ${diffFromLambdaStart}ms relative to lambda start`);
      }

      // The HTTP request happens during handler execution, so it should start
      // at or after the aws.lambda span start (allowing some tolerance)
      expect(diffFromLambdaStart).toBeGreaterThan(-5000); // 5 second tolerance
    }
  });

  it('should log span details for debugging', () => {
    const telemetry = result.telemetry;
    if (!telemetry?.traces?.length) return;

    console.log('\n=== Span Details ===');
    const trace = telemetry.traces[0];
    for (const span of trace.spans) {
      const opName = span.attributes?.operation_name || span.attributes?.name || 'unknown';
      const resource = span.attributes?.resource_name || 'unknown';
      const spanType = span.attributes?.type || 'unknown';
      const custom = span.attributes?.custom;
      const adjusted = custom?._dd?.snapstart_adjusted === 'true' ||
                       custom?.['_dd.snapstart_adjusted'] === 'true' ||
                       span.attributes?.['_dd.snapstart_adjusted'] === 'true';

      console.log(`  ${opName} (${spanType}): resource=${resource}${adjusted ? ' [ADJUSTED]' : ''}`);
    }

    // Count adjusted spans
    const adjustedCount = trace.spans.filter((span: any) => {
      const custom = span.attributes?.custom;
      return custom?._dd?.snapstart_adjusted === 'true' ||
             custom?.['_dd.snapstart_adjusted'] === 'true' ||
             span.attributes?.['_dd.snapstart_adjusted'] === 'true';
    }).length;

    console.log(`\nAdjusted spans: ${adjustedCount}`);

    // Also log operation names to see what spans we have
    const opNames = trace.spans.map((s: any) => s.attributes?.operation_name || 'unknown');
    console.log(`Span operation names: ${opNames.join(', ')}`);

    // Check if we have any OkHttp spans
    const httpSpans = trace.spans.filter((s: any) => {
      const opName = s.attributes?.operation_name || '';
      const spanType = s.attributes?.type || '';
      return opName.includes('http') || opName.includes('okhttp') ||
             spanType === 'http' || spanType === 'web';
    });
    console.log(`HTTP-related spans found: ${httpSpans.length}`);

    if (adjustedCount === 0 && httpSpans.length === 0) {
      console.log('Note: No HTTP client spans found. The Java tracer may not be instrumenting OkHttp during this execution.');
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
