import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData, DATADOG_INDEXING_WAIT_5_MIN_MS } from './utils/util';
import { getIdentifier } from '../config';

describe('OTLP Integration Tests', () => {
  const results: Record<string, LambdaInvocationDatadogData> = {};

  beforeAll(async () => {
    const identifier = getIdentifier();
    const functions = {
      node: `integ-${identifier}-otlp-node-lambda`,
      python: `integ-${identifier}-otlp-python-lambda`,
      java: `integ-${identifier}-otlp-java-lambda`,
      dotnet: `integ-${identifier}-otlp-dotnet-lambda`,
      responseValidation: `integ-${identifier}-otlp-response-validation-lambda`,
    };

    console.log('Invoking all OTLP Lambda functions in parallel...');

    // Invoke all Lambdas in parallel (using 5 minute wait for OTLP indexing)
    const invocationResults = await Promise.all([
      invokeLambdaAndGetDatadogData(functions.node, {}, true, true, DATADOG_INDEXING_WAIT_5_MIN_MS),
      invokeLambdaAndGetDatadogData(functions.python, {}, true, true, DATADOG_INDEXING_WAIT_5_MIN_MS),
      invokeLambdaAndGetDatadogData(functions.java, {}, true, true, DATADOG_INDEXING_WAIT_5_MIN_MS),
      invokeLambdaAndGetDatadogData(functions.dotnet, {}, true, true, DATADOG_INDEXING_WAIT_5_MIN_MS),
      invokeLambdaAndGetDatadogData(functions.responseValidation, {}, true, true, DATADOG_INDEXING_WAIT_5_MIN_MS),
    ]);

    // Store results
    results.node = invocationResults[0];
    results.python = invocationResults[1];
    results.java = invocationResults[2];
    results.dotnet = invocationResults[3];
    results.responseValidation = invocationResults[4];

    console.log('All OTLP Lambda invocations and data fetching completed');
  }, 700000); // 11.6 minute timeout

  describe('Node.js Runtime', () => {
    it('should invoke Node.js Lambda successfully', () => {
      expect(results.node.statusCode).toBe(200);
    });

    it('should send at least one trace to Datadog', () => {
      expect(results.node.traces?.length).toBeGreaterThan(0);
    });

    it('should have spans in the trace', () => {
      const trace = results.node.traces![0];
      expect(trace.spans.length).toBeGreaterThan(0);
    });
  });

  describe('Python Runtime', () => {
    it('should invoke Python Lambda successfully', () => {
      expect(results.python.statusCode).toBe(200);
    });

    it('should send at least one trace to Datadog', () => {
      expect(results.python.traces?.length).toBeGreaterThan(0);
    });

    it('should have spans in the trace', () => {
      const trace = results.python.traces![0];
      expect(trace.spans.length).toBeGreaterThan(0);
    });
  });

  describe('Java Runtime', () => {
    it('should invoke Java Lambda successfully', () => {
      expect(results.java.statusCode).toBe(200);
    });

    it('should send at least one trace to Datadog', () => {
      expect(results.java.traces?.length).toBeGreaterThan(0);
    });

    it('should have spans in the trace', () => {
      const trace = results.java.traces![0];
      expect(trace.spans.length).toBeGreaterThan(0);
    });
  });

  describe('.NET Runtime', () => {
    it('should invoke .NET Lambda successfully', () => {
      expect(results.dotnet.statusCode).toBe(200);
    });

    it('should send at least one trace to Datadog', () => {
      expect(results.dotnet.traces?.length).toBeGreaterThan(0);
    });

    it('should have spans in the trace', () => {
      const trace = results.dotnet.traces![0];
      expect(trace.spans.length).toBeGreaterThan(0);
    });
  });

  describe('OTLP Response Validation', () => {
    it('should invoke response validation Lambda successfully', () => {
      expect(results.responseValidation.statusCode).toBe(200);
    });

    it('should have JSON encoded span in Datadog', () => {
      const allSpans = results.responseValidation.traces?.flatMap(t => t.spans) || [];
      const hasJsonSpan = allSpans.some(s =>
        s.attributes?.resource_name === 'test-span-json' && s.attributes?.custom?.encoding === 'json'
      );
      expect(hasJsonSpan).toBe(true);
    });

    it('should have Protobuf encoded span in Datadog', () => {
      const allSpans = results.responseValidation.traces?.flatMap(t => t.spans) || [];
      const hasProtobufSpan = allSpans.some(s =>
        s.attributes?.resource_name === 'test-span-protobuf' && s.attributes?.custom?.encoding === 'protobuf'
      );
      expect(hasProtobufSpan).toBe(true);
    });
  });
});
