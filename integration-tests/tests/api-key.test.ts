import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { forceColdStart, publishVersion, waitForSnapStartReady } from './utils/lambda';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-api-key`;

describe('API Key Resolution Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  const getFirstInvocation = (runtime: string) => telemetry[runtime]?.threads[0]?.[0];

  beforeAll(async () => {
    const nodeFunctionName = `${stackName}-node`;
    const javaFunctionName = `${stackName}-java`;
    const ssmNodeFunctionName = `${stackName}-ssm-node`;

    await Promise.all([
      forceColdStart(nodeFunctionName),
      forceColdStart(ssmNodeFunctionName),
    ]);

    const javaVersion = await publishVersion(javaFunctionName);
    await waitForSnapStartReady(javaFunctionName, javaVersion);

    const functions: FunctionConfig[] = [
      { functionName: nodeFunctionName, runtime: 'node' },
      { functionName: `${javaFunctionName}:${javaVersion}`, runtime: 'java' },
      { functionName: ssmNodeFunctionName, runtime: 'ssm-node' },
    ];

    telemetry = await invokeAndCollectTelemetry(functions, 1);

    console.log('All invocations and data fetching completed');
  }, 600000);

  describe('delegated auth (node)', () => {
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

  describe('delegated auth (java, snapstart)', () => {
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

  describe('SSM Parameter Store API key (node)', () => {
    const getInvocation = () => getFirstInvocation('ssm-node');

    it('should invoke Lambda successfully when API key is sourced from SSM', () => {
      const result = getInvocation();
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should send logs to Datadog using API key fetched from SSM', () => {
      const result = getInvocation();
      expect(result).toBeDefined();
      expect(result.logs!.length).toBeGreaterThan(0);
    });

    it('should send traces to Datadog using API key fetched from SSM', () => {
      const result = getInvocation();
      expect(result).toBeDefined();
      expect(result.traces?.length).toBeGreaterThan(0);
    });
  });
});
