import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from '../config';

describe('LMI Integration Tests', () => {
  const results: Record<string, LambdaInvocationDatadogData> = {};

  const runtimes = [
    { runtime: 'node' },
    { runtime:  'python' },
    { runtime:  'java' },
    { runtime:  'dotnet' }
  ];

  beforeAll(async () => {
    const identifier = getIdentifier();

    console.log('Invoking LMI functions...');

    for (const runtime of runtimes) {
      const functionName = `integ-${identifier}-lmi-${runtime.runtime}-lambda:lmi`;
      results[runtime.runtime] = await invokeLambdaAndGetDatadogData(functionName, {}, false, false);
    }

    console.log('LMI invocation and data fetching completed');
  }, 600000);

  describe.each(runtimes)('$runtime Runtime with LMI', ({runtime} ) => {
    it('should invoke Lambda successfully', () => {
      expect(results[runtime].statusCode).toBe(200);
    });

    it('should have logs in Datadog', () => {
      expect(results[runtime].logs).toBeDefined();
      expect(results[runtime].logs!.length).toBeGreaterThan(0);
    });

    it('should have "Hello World!" log message', () => {
      expect(results[runtime].logs).toBeDefined();
      const helloWorldLog = results[runtime].logs!.find((log: any) =>
        log.message && log.message.includes('Hello World!')
      );
      expect(helloWorldLog).toBeDefined();
    });

    it('should send one trace to Datadog', () => {
      expect(results[runtime].traces?.length).toEqual(1);
    });


    it('trace should have exactly one span with operation_name=aws.lambda', () => {
      const trace = results[runtime].traces![0];
      expect(trace.spans).toBeDefined();

      // Filter spans with operation_name='aws.lambda'
      const awsLambdaSpans = trace.spans.filter((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpans).toBeDefined();
      // Verify exactly one span has operation_name='aws.lambda'
      expect(awsLambdaSpans.length).toEqual(1);
    });
    it('aws.lambda.span should have init_type set to lambda-managed-instances', () => {
      const trace = results[runtime].traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan?.attributes.custom.init_type).toBe('lambda-managed-instances')
    })
    // SVLS-8232
    // In Managed Instance mode, cold_start span and tag are not sent by bottlecap
    // because the concept is less meaningful with concurrent invocations
    // and it would create a poor flame graph experience due to time gaps.
    //
    // Note: Node and Python tracers (dd-trace-js, dd-trace-py) set their own cold_start
    // attribute on spans independently, so we skip the tag check for those runtimes.
    it('aws.lambda.span should NOT have cold_start span or tag in LMI mode', () => {
      const trace = results[runtime].traces![0];

      // Verify no 'aws.lambda.cold_start' span exists in LMI mode
      const coldStartSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda.cold_start'
      );
      expect(coldStartSpan).toBeUndefined();

      const awsLambdaSpan = trace.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      expect(awsLambdaSpan).toBeDefined(); // Ensure span exists before checking attributes

      // Skip cold_start tag check for Node and Python since their tracer libraries
      // (dd-trace-js, dd-trace-py) set the cold_start attribute independently.
      // Only verify for Java and dotnet where bottlecap controls the span.
      if (runtime !== 'node' && runtime !== 'python') {
        expect(awsLambdaSpan?.attributes.custom.cold_start).toBeUndefined();
      }
    })

  });
});
