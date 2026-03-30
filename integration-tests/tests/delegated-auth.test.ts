import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { forceColdStart } from './utils/lambda';
import { getIdentifier } from '../config';

const identifier = getIdentifier();
const stackName = `integ-${identifier}-delegated-auth`;

const FUNCTION_NAME = stackName;

describe('Delegated Authentication Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  const getFirstInvocation = () => telemetry['delegated-auth']?.threads[0]?.[0];

  beforeAll(async () => {
    const functions: FunctionConfig[] = [{
      functionName: FUNCTION_NAME,
      runtime: 'delegated-auth',
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

  it('should NOT show fallback to static API key', () => {
    const result = getFirstInvocation();
    expect(result).toBeDefined();

    const fallbackLog = result.logs?.find((log: any) =>
      log.message.includes('fallback') || log.message.includes('Falling back')
    );
    expect(fallbackLog).toBeUndefined();
  });

  it('should send telemetry to Datadog (validates API key works)', () => {
    const result = getFirstInvocation();
    expect(result).toBeDefined();
    expect(result.logs!.length).toBeGreaterThan(0);
  });
});
