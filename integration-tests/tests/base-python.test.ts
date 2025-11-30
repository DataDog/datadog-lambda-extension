import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';

describe('Base Python Lambda Integration Test', () => {
  const PYTHON_FUNCTION_NAME = `integ-${getIdentifier()}-base-python-lambda`;
  let result: LambdaInvocationDatadogData;

  beforeAll(async () => {
    console.log(`Invoking Lambda function: ${PYTHON_FUNCTION_NAME}`);
    result = await invokeLambdaAndGetDatadogData(PYTHON_FUNCTION_NAME, {}, true);
  }, 700000); // 11.6 minute timeout

  it('should invoke Python Lambda successfully', () => {
    expect(result.statusCode).toBe(200);
  });

  it('should have "Hello world!" log message', () => {
    const helloWorldLog = result.logs?.find((log: any) =>
      log.attributes.message.includes('Hello world!')
    );
    expect(helloWorldLog).toBeDefined();
    console.log('Hello world log:', JSON.stringify(helloWorldLog, null, 2));
  });

  it('should send one trace to Datadog', () => {
    expect(result.traces?.length).toEqual(1);
  });

  it('should have aws.lambda span', () => {
    const trace = result.traces![0];
    const awsLambdaSpan = trace.spans.find((span: any) => span.name === 'aws.lambda');
    expect(awsLambdaSpan).toBeDefined();
  });

  // TODO: These spans are being created but not with the same traceId as the 'aws.lambda' span
  //       Need to investigate why this is happening and fix it.
  it.failing('[failing] should have aws.lambda.cold_start span', () => {
    const trace = result.traces![0];
    const awsLambdaColdStartSpan = trace.spans.find((span: any) => span.name === 'aws.lambda.cold_start');
    expect(awsLambdaColdStartSpan).toBeDefined();
  });

  // TODO: These spans are being created but not with the same traceId as the 'aws.lambda' span
  //       Need to investigate why this is happening and fix it.
  it.failing('[failing] should have aws.lambda.load span', () => {
    const trace = result.traces![0];
    const awsLambdaLoadSpan = trace.spans.find((span: any) => span.name === 'aws.lambda.load');
    expect(awsLambdaLoadSpan).toBeDefined();
  });
});
