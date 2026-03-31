import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { forceColdStart } from './utils/lambda';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-auth`;

describe('Auth Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  const getFirstInvocation = (runtime: string) => telemetry[runtime]?.threads[0]?.[0];

  beforeAll(async () => {
    const functions: FunctionConfig[] = [
      { functionName: `${stackName}-node`, runtime: 'node' },
      { functionName: `${stackName}-java:snapstart`, runtime: 'java' },
    ];

    await Promise.all(functions.map(fn => forceColdStart(fn.functionName)));

    telemetry = await invokeAndCollectTelemetry(functions, 1);

    console.log('All invocations and data fetching completed');
  }, 600000);

  describe('on-demand (node)', () => {
    it('should invoke Lambda successfully', () => {
      const result = getFirstInvocation('node');
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should send logs to Datadog via delegated auth', () => {
      const result = getFirstInvocation('node');
      expect(result).toBeDefined();
      expect(result.logs!.length).toBeGreaterThan(0);
    });
  });

  describe('snapstart (java)', () => {
    it('should invoke Lambda successfully', () => {
      const result = getFirstInvocation('java');
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should send logs to Datadog via delegated auth', () => {
      const result = getFirstInvocation('java');
      expect(result).toBeDefined();
      expect(result.logs!.length).toBeGreaterThan(0);
    });
  });
});
