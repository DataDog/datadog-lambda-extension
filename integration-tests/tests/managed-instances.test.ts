import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from '../config';

describe('Lambda Managed Instances Integration Tests', () => {
  const results: Record<string, LambdaInvocationDatadogData> = {};

  const runtimes = [
    { runtime: 'node' },
    { runtime:  'python' },
    { runtime:  'java' },
    { runtime:  'dotnet' }
  ];

  beforeAll(async () => {
    const identifier = getIdentifier();

    console.log('Invoking Lambda Managed Instances functions...');

    for (const runtime of runtimes) {
      const functionName = `integ-${identifier}-lmi-${runtime.runtime}-lambda:managed-instances`;
      results[runtime.runtime] = await invokeLambdaAndGetDatadogData(functionName, {}, false, false);
    }

    console.log('Lambda Managed Instances invocation and data fetching completed');
  }, 600000);

  describe.each(runtimes)('$runtime Runtime with Lambda Managed Instances', ({runtime} ) => {
    it('should invoke Lambda successfully', () => {
      expect(results[runtime].statusCode).toBe(200);
    });

    it('should have logs in Datadog', () => {
      expect(results[runtime].logs).toBeDefined();
      expect(results[runtime].logs!.length).toBeGreaterThan(0);
    });

    // SVLS-8231
    test.failing('should have "Hello World!" log message', () => {
      expect(results[runtime].logs).toBeDefined();
      const helloWorldLog = results[runtime].logs!.find((log: any) =>
        log.message && log.message.includes('Hello World!')
      );
      expect(helloWorldLog).toBeDefined();
    });

    it('should send one trace to Datadog', () => {
      expect(results[runtime].traces?.length).toEqual(1);
    });

    // SVLS-8232
    test.skip('trace should have exactly one span', () => {
      const trace = results[runtime].traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      expect(trace.spans).toBeDefined();
      expect(trace.spans.length).toEqual(1);
    });
    it('trace should have aws.lambda span', () => {
      const trace = results[runtime].traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined();
    });
    it('aws.lambda.span should have init_type set to lambda-managed-instances', () => {
      const trace = results[runtime].traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan?.attributes.custom.init_type).toBe('lambda-managed-instances')
    })
    // SVLS-8232
    test.skip('aws.lambda.span should have cold_start set to false', () => {
      const trace = results[runtime].traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan?.attributes.custom.cold_start).toBe('false')
    })

  });
});
