import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';

describe('Base Java Lambda Integration Test', () => {
  const JAVA_FUNCTION_NAME = `integ-${getIdentifier()}-base-java-lambda`;
  let result: LambdaInvocationDatadogData;

  beforeAll(async () => {
    console.log(`Invoking Lambda function: ${JAVA_FUNCTION_NAME}`);
    result = await invokeLambdaAndGetDatadogData(JAVA_FUNCTION_NAME, {}, true);
    console.log(`Found ${result.logs?.length} logs`);
  }, 700000); // 11.6 minute timeout

  it('should invoke Java Lambda successfully', () => {
    expect(result.statusCode).toBe(200);
  });

  it('should have "Hello world!" log message', () => {
    const helloWorldLog = result.logs?.find((log: any) =>
      log.attributes.message.includes('Hello world!')
    );
    expect(helloWorldLog).toBeDefined();
    console.log('✅ Hello world log found');
  });

  it('should send one trace to Datadog', () => {
    expect(result.traces?.length).toEqual(1);
    console.log('✅ One trace sent to Datadog');
  });

  it('should have aws.lambda span', () => {
    const trace = result.traces![0];
    const awsLambdaSpan = trace.spans.find((span: any) => span.name === 'aws.lambda');
    expect(awsLambdaSpan).toBeDefined();
    console.log('✅ aws.lambda span found');
  });

  it('should have aws.lambda.cold_start span', () => {
    const trace = result.traces![0];
    const awsLambdaColdStartSpan = trace.spans.find((span: any) => span.name === 'aws.lambda.cold_start');
    expect(awsLambdaColdStartSpan).toBeDefined();
    console.log('✅ aws.lambda.cold_start span found');
  });

});
