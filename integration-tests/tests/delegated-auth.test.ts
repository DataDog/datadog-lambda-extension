import { invokeLambda, forceColdStart } from './utils/lambda';
import { getTraces, getLogs, DatadogTrace, DatadogLog } from './utils/datadog';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-delegated-auth`;

// Function name matches the CDK stack id
const FUNCTION_NAME = stackName;

// Default wait time for Datadog to index logs and traces after Lambda invocation
const DEFAULT_DATADOG_INDEXING_WAIT_MS = 5 * 60 * 1000; // 5 minutes

async function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

interface DatadogTelemetry {
  requestId: string;
  statusCode?: number;
  traces: DatadogTrace[];
  logs: DatadogLog[];
}

async function getDatadogTelemetryByRequestId(
  functionName: string,
  requestId: string
): Promise<DatadogTelemetry> {
  const traces = await getTraces(functionName, requestId);
  const logs = await getLogs(functionName, requestId);
  return { requestId, traces, logs };
}

describe('Delegated Authentication Integration Tests', () => {
  let invocationResult: { requestId: string; statusCode?: number };
  let telemetry: DatadogTelemetry;
  let logs: string[];

  beforeAll(async () => {
    console.log(`Testing delegated auth function: ${FUNCTION_NAME}`);

    // Force cold start to ensure extension initializes fresh
    await forceColdStart(FUNCTION_NAME);

    // Invoke the function
    invocationResult = await invokeLambda(FUNCTION_NAME, {});

    console.log(`Invocation completed, requestId: ${invocationResult.requestId}`);

    // Wait for telemetry to be indexed in Datadog
    console.log(`Waiting ${DEFAULT_DATADOG_INDEXING_WAIT_MS / 1000}s for Datadog indexing...`);
    await sleep(DEFAULT_DATADOG_INDEXING_WAIT_MS);

    // Collect telemetry from Datadog
    telemetry = await getDatadogTelemetryByRequestId(
      FUNCTION_NAME,
      invocationResult.requestId
    );
    logs = telemetry.logs.map((log: DatadogLog) => log.message);

    console.log(`Collected ${telemetry.logs.length} logs and ${telemetry.traces.length} traces`);
  }, 600000); // 10 minute timeout

  it('should invoke Lambda successfully', () => {
    expect(invocationResult).toBeDefined();
    expect(invocationResult.statusCode).toBe(200);
  });

  it('should have function log output', () => {
    expect(telemetry).toBeDefined();
    expect(telemetry.logs).toBeDefined();
    expect(telemetry.logs.length).toBeGreaterThan(0);
  });

  it('should show delegated auth API key obtained successfully', () => {
    const delegatedAuthLog = logs.find((log: string) =>
      log.includes('Delegated auth') &&
      (log.includes('API key obtained') || log.includes('success'))
    );
    expect(delegatedAuthLog).toBeDefined();
  });

  it('should NOT show fallback to static API key', () => {
    const fallbackLog = logs.find((log: string) =>
      log.includes('fallback') || log.includes('Falling back')
    );
    expect(fallbackLog).toBeUndefined();
  });

  it('should send telemetry to Datadog (validates API key works)', () => {
    expect(telemetry.logs).toBeDefined();
    expect(telemetry.logs.length).toBeGreaterThan(0);
  });

});
