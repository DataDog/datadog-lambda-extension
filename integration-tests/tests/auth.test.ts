import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { forceColdStart } from './utils/lambda';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-auth`;

const FUNCTION_NAME = stackName;

describe('Auth Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  const getFirstInvocation = () => telemetry['auth']?.threads[0]?.[0];

  beforeAll(async () => {
    const functions: FunctionConfig[] = [{
      functionName: FUNCTION_NAME,
      runtime: 'auth',
    }];

    await forceColdStart(FUNCTION_NAME);

    telemetry = await invokeAndCollectTelemetry(functions, 1);

    console.log('All invocations and data fetching completed');
  }, 600000);

  it('should invoke Lambda successfully', () => {
    const result = getFirstInvocation();
    expect(result).toBeDefined();
    expect(result.statusCode).toBe(200);
  });

  it('should have function log output', () => {
    const result = getFirstInvocation();
    expect(result).toBeDefined();
    expect(result.logs!.length).toBeGreaterThan(0);
  });

  it('should send telemetry to Datadog (validates API key works)', () => {
    const result = getFirstInvocation();
    expect(result).toBeDefined();
    expect(result.logs!.length).toBeGreaterThan(0);
  });
});
