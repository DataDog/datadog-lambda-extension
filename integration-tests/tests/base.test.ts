import { invokeLambdaAndGetDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';

describe('Example Lambda Integration Test', () => {
  const FUNCTION_NAME = `integ-${getIdentifier()}-base-node-function`;

  it('should invoke Lambda successfully and receive logs in Datadog', async () => {

    // Step 1: Invoke the Lambda function with a cold start
    console.log(`Invoking Lambda function: ${FUNCTION_NAME}`);
    const result = await invokeLambdaAndGetDatadogData(FUNCTION_NAME, {}, true);

    // Step 2: Verify the Lambda invocation was successful
    expect(result.statusCode).toBe(200);

    // Step 3: Verify logs were sent to Datadog and contain the expected content
    const logs = result.logs;
    expect(logs?.length).toBeGreaterThanOrEqual(1);
    expect(logs?.some((log: any) => log.attributes.message.includes('Hello world!'))).toBe(true);

    // Step 4: Verify traces were sent to Datadog and contain the expected content
    const traces = result.traces;
    expect(traces?.length).toBe(1);

    const trace = traces![0];
    const spanNames = trace.spans.map((span: any) => span.name);
    console.log('Span names:', spanNames);

    expect(spanNames).toContain('aws.lambda.cold_start');
    expect(spanNames).toContain('aws.lambda.load');
    expect(spanNames).toContain('aws.lambda');

    console.log('✅ Example Lambda test passed! Logs successfully sent via extension and appeared in Datadog');
  }, 700000); // 11.6 minute timeout (700 seconds)
  
});
