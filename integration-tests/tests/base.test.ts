import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from '../config';

describe('Base Integration Tests', () => {
  const results: Record<string, LambdaInvocationDatadogData> = {};

  beforeAll(async () => {
    const identifier = getIdentifier();
    const functions = {
      node: `integ-${identifier}-base-node-lambda`,
      python: `integ-${identifier}-base-python-lambda`,
      java: `integ-${identifier}-base-java-lambda`,
      dotnet: `integ-${identifier}-base-dotnet-lambda`,
    };

    console.log('Invoking all base Lambda functions in parallel...');

    // Invoke all Lambdas in parallel
    const invocationResults = await Promise.all([
      invokeLambdaAndGetDatadogData(functions.node, {}, true),
      invokeLambdaAndGetDatadogData(functions.python, {}, true),
      invokeLambdaAndGetDatadogData(functions.java, {}, true),
      invokeLambdaAndGetDatadogData(functions.dotnet, {}, true),
    ]);

    // Store results
    results.node = invocationResults[0];
    results.python = invocationResults[1];
    results.java = invocationResults[2];
    results.dotnet = invocationResults[3];

    console.log('All base Lambda invocations and data fetching completed');
  }, 700000); // 11.6 minute timeout

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
