import { invokeLambdaAndGetDatadogData } from './utils/util';
import { getFunctionName } from './utils/config';

describe('Example Lambda Integration Test', () => {
  const FUNCTION_NAME = getFunctionName('exampleTestFunction');

  it('should invoke Lambda successfully and receive logs in Datadog', async () => {

    // Step 1: Invoke the Lambda function with a cold start
    console.log(`Invoking Lambda function: ${FUNCTION_NAME}`);
    const result = await invokeLambdaAndGetDatadogData(FUNCTION_NAME, {}, true);

    // Step 2: Verify the Lambda invocation was successful
    expect(result.statusCode).toBe(200);
    expect(result.payload.statusCode).toBe(200);
    expect(result.payload.body).toContain('Hello world!');

    console.log(`Lambda invoked successfully. RequestId: ${result.requestId}`);

    // Step 3: Verify logs were sent to Datadog and contain the expected content
    const logs = result.logs;
    console.log('Logs:', JSON.stringify(logs, null, 2));

    expect(logs?.length).toBeGreaterThanOrEqual(1);
    expect(logs?.some((log: any) => log.attributes.message.includes('Hello world!'))).toBe(true);

    console.log('✅ Example Lambda test passed! Logs successfully sent via extension and appeared in Datadog');
  }, 700000); // 11.6 minute timeout (700 seconds)
  
});
