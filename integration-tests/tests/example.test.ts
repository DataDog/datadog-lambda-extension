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
    expect(result.payload.body).toContain('Hello world!');

    // Step 3: Verify logs were sent to Datadog and contain the expected content
    const logs = result.logs;
    console.log('Logs:', JSON.stringify(logs, null, 2));

    expect(logs?.length).toBeGreaterThanOrEqual(1);
    expect(logs?.some((log: any) => log.attributes.message.includes('Hello world!'))).toBe(true);

    // Step 4: Verify traces were sent to Datadog and contain the expected content
    const traces = result.traces;
    console.log('========== TRACES DEBUG INFO ==========');
    console.log('Number of traces:', traces?.length);
    console.log('Full traces object:', JSON.stringify(traces, null, 2));

    if (traces && traces.length > 0) {
      traces.forEach((trace: any, traceIndex: number) => {
        console.log(`\n--- Trace ${traceIndex + 1} ---`);
        console.log('Trace ID:', trace.trace_id);
        console.log('Number of spans:', trace.spans.length);

        trace.spans.forEach((span: any, spanIndex: number) => {
          console.log(`\n  Span ${spanIndex + 1}:`);
          console.log('    Name:', span.name);
          console.log('    Resource:', span.resource);
          console.log('    Service:', span.service);
          console.log('    Span ID:', span.span_id);
        });
      });
    }
    console.log('========================================\n');

    // Assertions for traces
    expect(traces?.length).toBe(1);

    const trace = traces![0];
    const spanNames = trace.spans.map((span: any) => span.name);
    console.log('Span names:', spanNames);

    // Should contain 'aws.lambda.cold_start' and 'aws.lambda' spans
    expect(spanNames).toContain('aws.lambda.cold_start');
    expect(spanNames).toContain('aws.lambda.load');
    expect(spanNames).toContain('aws.lambda');

    console.log('✅ Example Lambda test passed! Logs successfully sent via extension and appeared in Datadog');
  }, 700000); // 11.6 minute timeout (700 seconds)
  
});
