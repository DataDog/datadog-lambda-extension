import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { invokeLambda, LambdaInvocationResult } from './utils/lambda';
import { getTraces } from './utils/datadog';
import { getIdentifier } from '../config';

describe('Base Integration Tests', () => {
  const results: Record<string, LambdaInvocationDatadogData> = {};
  const secondInvokeResults: Record<string, LambdaInvocationDatadogData> = {};

  beforeAll(async () => {
    const identifier = getIdentifier();
    const functions = {
      node: `integ-${identifier}-base-node-lambda`,
      python: `integ-${identifier}-base-python-lambda`,
      java: `integ-${identifier}-base-java-lambda`,
      dotnet: `integ-${identifier}-base-dotnet-lambda`,
    };

    // Helper to invoke twice rapidly (cold start + warm start 10ms later) then fetch traces
    async function invokeWithRapidSecondCall(functionName: string): Promise<{
      first: LambdaInvocationDatadogData;
      second: LambdaInvocationDatadogData;
    }> {
      // First invocation (cold start)
      const firstResult = await invokeLambda(functionName, {}, true);

      // Wait 10ms then invoke again (warm start) - SVLS-8659 regression test
      await new Promise(resolve => setTimeout(resolve, 10));
      const secondResult = await invokeLambda(functionName, {}, false);

      // Wait for Datadog to index
      console.log(`Waiting 60s for Datadog to index ${functionName}...`);
      await new Promise(resolve => setTimeout(resolve, 60000));

      // Fetch traces for both invocations
      const baseFunctionName = functionName.split(':')[0];
      const [firstTraces, secondTraces] = await Promise.all([
        getTraces(baseFunctionName, firstResult.requestId),
        getTraces(baseFunctionName, secondResult.requestId),
      ]);

      return {
        first: {
          requestId: firstResult.requestId,
          statusCode: firstResult.statusCode,
          payload: firstResult.payload,
          traces: firstTraces,
        },
        second: {
          requestId: secondResult.requestId,
          statusCode: secondResult.statusCode,
          payload: secondResult.payload,
          traces: secondTraces,
        },
      };
    }

    console.log('Invoking all base Lambda functions with rapid second invocation...');

    // Invoke all Lambdas in parallel, each with rapid second invocation
    const invocationResults = await Promise.all([
      invokeWithRapidSecondCall(functions.node),
      invokeWithRapidSecondCall(functions.python),
      invokeWithRapidSecondCall(functions.java),
      invokeWithRapidSecondCall(functions.dotnet),
    ]);

    // Store results
    results.node = invocationResults[0].first;
    results.python = invocationResults[1].first;
    results.java = invocationResults[2].first;
    results.dotnet = invocationResults[3].first;

    secondInvokeResults.node = invocationResults[0].second;
    secondInvokeResults.python = invocationResults[1].second;
    secondInvokeResults.java = invocationResults[2].second;
    secondInvokeResults.dotnet = invocationResults[3].second;

    console.log('All base Lambda invocations and data fetching completed');
  }, 900000); // 15 minute timeout

  describe('Node.js Runtime', () => {
    it('should invoke Node.js Lambda successfully', () => {
      expect(results.node.statusCode).toBe(200);
    });

    it('should have "Hello world!" log message', () => {
      const helloWorldLog = results.node.logs?.find((log: any) =>
        log.message.includes('Hello world!')
      );
      expect(helloWorldLog).toBeDefined();
    });

    it('should send one trace to Datadog', () => {
      expect(results.node.traces?.length).toEqual(1);
    });

    it('should have aws.lambda span with correct properties', () => {
      const trace = results.node.traces![0];
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
      const trace = results.node.traces![0];
      const awsLambdaColdStartSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.cold_start');
      expect(awsLambdaColdStartSpan).toBeDefined();
      expect(awsLambdaColdStartSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda.cold_start',
        }
      });
    });

    it('should have aws.lambda.load span', () => {
      const trace = results.node.traces![0];
      const awsLambdaLoadSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.load');
      expect(awsLambdaLoadSpan).toBeDefined();
      expect(awsLambdaLoadSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda.load',
        }
      });
    });
  });

  describe('Python Runtime', () => {
    it('should invoke Python Lambda successfully', () => {
      expect(results.python.statusCode).toBe(200);
    });

    it('should have "Hello world!" log message', () => {
      const helloWorldLog = results.python.logs?.find((log: any) =>
        log.message.includes('Hello world!')
      );
      expect(helloWorldLog).toBeDefined();
    });

    it('should send one trace to Datadog', () => {
      expect(results.python.traces?.length).toEqual(1);
    });

    it('should have aws.lambda span with correct properties', () => {
      const trace = results.python.traces![0];
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

    // TODO: These spans are being created but not with the same traceId as the 'aws.lambda' span
    //       Need to investigate why this is happening and fix it.
    it.failing('[failing] should have aws.lambda.cold_start span', () => {
      const trace = results.python.traces![0];
      const awsLambdaColdStartSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.cold_start');
      expect(awsLambdaColdStartSpan).toBeDefined();
      expect(awsLambdaColdStartSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda.cold_start',
        }
      });
    });

    // TODO: These spans are being created but not with the same traceId as the 'aws.lambda' span
    //       Need to investigate why this is happening and fix it.
    it.failing('[failing] should have aws.lambda.load span', () => {
      const trace = results.python.traces![0];
      const awsLambdaLoadSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.load');
      expect(awsLambdaLoadSpan).toBeDefined();
      expect(awsLambdaLoadSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda.load',
        }
      });
    });
  });

  describe('Python Runtime - Second Invocation (SVLS-8659)', () => {
    it('should invoke Python Lambda successfully on second invocation', () => {
      expect(secondInvokeResults.python.statusCode).toBe(200);
    });

    it('should have aws.lambda span with cold_start=false on second invocation', () => {
      const trace = secondInvokeResults.python.traces![0];
      const awsLambdaSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda');
      expect(awsLambdaSpan).toBeDefined();
      expect(awsLambdaSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda',
          custom: {
            cold_start: 'false'
          }
        }
      });
    });

    it('should NOT have aws.lambda.cold_start span on second invocation', () => {
      const trace = secondInvokeResults.python.traces![0];
      const coldStartSpans = trace.spans.filter((span: any) => span.attributes.operation_name === 'aws.lambda.cold_start');
      // Second invocation (10ms after cold start) should NOT have a cold_start span
      // Before SVLS-8659 fix: would have duplicate cold_start span
      expect(coldStartSpans.length).toBe(0);
    });
  });

  describe('Java Runtime', () => {
    it('should invoke Java Lambda successfully', () => {
      expect(results.java.statusCode).toBe(200);
    });

    it('should have "Hello world!" log message', () => {
      const helloWorldLog = results.java.logs?.find((log: any) =>
        log.message.includes('Hello world!')
      );
      expect(helloWorldLog).toBeDefined();
    });

    it('should send one trace to Datadog', () => {
      expect(results.java.traces?.length).toEqual(1);
    });

    it('should have aws.lambda span with correct properties', () => {
      const trace = results.java.traces![0];
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
      const trace = results.java.traces![0];
      const awsLambdaColdStartSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.cold_start');
      expect(awsLambdaColdStartSpan).toBeDefined();
      expect(awsLambdaColdStartSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda.cold_start',
        }
      });
    });
  });

  describe('.NET Runtime', () => {
    it('should invoke .NET Lambda successfully', () => {
      expect(results.dotnet.statusCode).toBe(200);
    });

    it('should have "Hello world!" log message', () => {
      const helloWorldLog = results.dotnet.logs?.find((log: any) =>
        log.message.includes('Hello world!')
      );
      expect(helloWorldLog).toBeDefined();
    });

    it('should send one trace to Datadog', () => {
      expect(results.dotnet.traces?.length).toEqual(1);
    });

    it('should have aws.lambda span with correct properties', () => {
      const trace = results.dotnet.traces![0];
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
      const trace = results.dotnet.traces![0];
      const awsLambdaColdStartSpan = trace.spans.find((span: any) => span.attributes.operation_name === 'aws.lambda.cold_start');
      expect(awsLambdaColdStartSpan).toBeDefined();
      expect(awsLambdaColdStartSpan).toMatchObject({
        attributes: {
          operation_name: 'aws.lambda.cold_start',
        }
      });
    });

  });
});
