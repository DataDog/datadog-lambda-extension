import { invokeLambda, forceColdStart } from './utils/lambda';
import { getTraces, getLogs, DatadogTrace, DatadogLog } from './utils/datadog';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-delegated-auth`;

// Function names from CDK stack
const HAPPY_PATH_FUNCTION = `${stackName}-happy-path`;
const FALLBACK_FUNCTION = `${stackName}-fallback`;

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

  describe('Happy Path - Delegated Auth Success', () => {
    let invocationResult: { requestId: string; statusCode?: number };
    let telemetry: DatadogTelemetry;
    let logs: string[];

    beforeAll(async () => {
      console.log(`Testing happy path function: ${HAPPY_PATH_FUNCTION}`);

      // Force cold start to ensure extension initializes fresh
      await forceColdStart(HAPPY_PATH_FUNCTION);

      // Invoke the function
      invocationResult = await invokeLambda(HAPPY_PATH_FUNCTION, {});

      console.log(`Invocation completed, requestId: ${invocationResult.requestId}`);

      // Wait for telemetry to be indexed in Datadog
      console.log(`Waiting ${DEFAULT_DATADOG_INDEXING_WAIT_MS / 1000}s for Datadog indexing...`);
      await sleep(DEFAULT_DATADOG_INDEXING_WAIT_MS);

      // Collect telemetry from Datadog
      telemetry = await getDatadogTelemetryByRequestId(
        HAPPY_PATH_FUNCTION,
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
      // Look for log message indicating delegated auth succeeded
      const delegatedAuthLog = logs.find((log: string) =>
        log.includes('Delegated auth') &&
        (log.includes('API key obtained') || log.includes('success'))
      );
      expect(delegatedAuthLog).toBeDefined();
    });

    it('should NOT show fallback to static API key', () => {
      // Ensure no fallback occurred
      const fallbackLog = logs.find((log: string) =>
        log.includes('fallback') || log.includes('Falling back')
      );
      expect(fallbackLog).toBeUndefined();
    });

    it('should send telemetry to Datadog (validates API key works)', () => {
      // If we have logs in Datadog, the obtained API key is working
      expect(telemetry.logs).toBeDefined();
      expect(telemetry.logs.length).toBeGreaterThan(0);
    });

    it('should send at least one trace to Datadog', () => {
      // Traces indicate the extension is functioning correctly
      expect(telemetry.traces).toBeDefined();
      expect(telemetry.traces.length).toBeGreaterThanOrEqual(1);
    });
  });

  describe('Fallback Path - Invalid Org UUID Falls Back to Static Key', () => {
    let invocationResult: { requestId: string; statusCode?: number };
    let telemetry: DatadogTelemetry;
    let logs: string[];

    beforeAll(async () => {
      console.log(`Testing fallback function: ${FALLBACK_FUNCTION}`);

      // Force cold start to ensure extension initializes fresh
      await forceColdStart(FALLBACK_FUNCTION);

      // Invoke the function
      invocationResult = await invokeLambda(FALLBACK_FUNCTION, {});

      console.log(`Invocation completed, requestId: ${invocationResult.requestId}`);

      // Wait for telemetry to be indexed in Datadog
      console.log(`Waiting ${DEFAULT_DATADOG_INDEXING_WAIT_MS / 1000}s for Datadog indexing...`);
      await sleep(DEFAULT_DATADOG_INDEXING_WAIT_MS);

      // Collect telemetry from Datadog
      telemetry = await getDatadogTelemetryByRequestId(
        FALLBACK_FUNCTION,
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

    it('should show delegated auth failure', () => {
      // Look for log message indicating delegated auth failed
      const failureLog = logs.find((log: string) =>
        (log.includes('Delegated auth') || log.includes('delegated auth')) &&
        (log.includes('fail') || log.includes('error') || log.includes('Error'))
      );
      expect(failureLog).toBeDefined();
    });

    it('should show fallback to static API key', () => {
      // Look for log message indicating fallback occurred
      const fallbackLog = logs.find((log: string) =>
        log.includes('fallback') || log.includes('Falling back') || log.includes('using static')
      );
      expect(fallbackLog).toBeDefined();
    });

    it('should still send telemetry to Datadog (via fallback key)', () => {
      // Even with delegated auth failure, telemetry should work via fallback
      expect(telemetry.logs).toBeDefined();
      expect(telemetry.logs.length).toBeGreaterThan(0);
    });

    it('should still send traces to Datadog (via fallback key)', () => {
      // Traces should still work via the fallback static API key
      expect(telemetry.traces).toBeDefined();
      expect(telemetry.traces.length).toBeGreaterThanOrEqual(1);
    });
  });
});
