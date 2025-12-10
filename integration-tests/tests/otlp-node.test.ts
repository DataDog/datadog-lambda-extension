import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';

describe('OTLP Node.js Lambda Integration Test', () => {
  const NODE_FUNCTION_NAME = `integ-${getIdentifier()}-otlp-node-lambda`;
  let result: LambdaInvocationDatadogData;

  beforeAll(async () => {
    console.log(`Invoking Lambda function: ${NODE_FUNCTION_NAME}`);
    result = await invokeLambdaAndGetDatadogData(NODE_FUNCTION_NAME, {}, true);
  }, 700000); // 11.6 minute timeout

  it('should invoke Node.js Lambda successfully', () => {
    expect(result.statusCode).toBe(200);
  });

  it('should send at least one trace to Datadog', () => {
    expect(result.traces?.length).toBeGreaterThan(0);
  });

  it('should have spans in the trace', () => {
    const trace = result.traces![0];
    expect(trace.spans.length).toBeGreaterThan(0);
  });
});
