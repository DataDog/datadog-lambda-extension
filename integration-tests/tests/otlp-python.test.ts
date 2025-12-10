import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';

describe('OTLP Python Lambda Integration Test', () => {
  const PYTHON_FUNCTION_NAME = `integ-${getIdentifier()}-otlp-python-lambda`;
  let result: LambdaInvocationDatadogData;

  beforeAll(async () => {
    console.log(`Invoking Lambda function: ${PYTHON_FUNCTION_NAME}`);
    result = await invokeLambdaAndGetDatadogData(PYTHON_FUNCTION_NAME, {}, true);
  }, 700000); // 11.6 minute timeout

  it('should invoke Python Lambda successfully', () => {
    expect(result.statusCode).toBe(200);
  });

  it('should have "Hello from OTLP Python!" log message', () => {
    const helloWorldLog = result.logs?.find((log: any) =>
      log.message.includes('Hello from OTLP Python!')
    );
    expect(helloWorldLog).toBeDefined();
  });

  it('should send at least one trace to Datadog', () => {
    expect(result.traces?.length).toBeGreaterThan(0);
  });

  it('should have spans in the trace', () => {
    const trace = result.traces![0];
    expect(trace.spans.length).toBeGreaterThan(0);
  });
});
