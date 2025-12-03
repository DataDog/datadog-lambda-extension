import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';

describe('Base Node Lambda Integration Test', () => {
  const NODE_FUNCTION_NAME = `integ-${getIdentifier()}-base-node-lambda`;
  let result: LambdaInvocationDatadogData;

  beforeAll(async () => {
    console.log(`Invoking Lambda function: ${NODE_FUNCTION_NAME}`);
    result = await invokeLambdaAndGetDatadogData(NODE_FUNCTION_NAME, {}, true);
  }, 700000); // 11.6 minute timeout

  it('should invoke Node.js Lambda successfully', () => {
    expect(result.statusCode).toBe(200);
  });

  it('should have "Hello world!" log message', () => {
    const helloWorldLog = result.logs?.find((log: any) =>
      log.message.includes('Hello world!')
    );
    expect(helloWorldLog).toBeDefined();
  });

  it('should send one trace to Datadog', () => {
    expect(result.traces?.length).toEqual(1);
  });

  it('should have aws.lambda span with correct properties', () => {
    const trace = result.traces![0];
    const awsLambdaSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda');
    expect(awsLambdaSpan).toBeDefined();
    expect(awsLambdaSpan).toMatchObject({
      attributes: {
        operation_name: 'aws.lambda',
        custom: {
          cold_start: 'true'
        }
      }
    });
  });

  it('should have aws.lambda.cold_start span', () => {
    const trace = result.traces![0];
    const awsLambdaColdStartSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.cold_start');
    expect(awsLambdaColdStartSpan).toBeDefined();
    expect(awsLambdaColdStartSpan).toMatchObject({
      attributes: {
        operation_name: 'aws.lambda.cold_start',
      }
    });
  });

  it('should have aws.lambda.load span', () => {
    const trace = result.traces![0];
    const awsLambdaLoadSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.load');
    expect(awsLambdaLoadSpan).toBeDefined();
    expect(awsLambdaLoadSpan).toMatchObject({
      attributes: {
        operation_name: 'aws.lambda.load',
      }
    });
  });
});
