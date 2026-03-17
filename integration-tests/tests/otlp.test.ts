import { invokeAndCollectTelemetry, FunctionConfig } from './utils/default';
import { DatadogTelemetry } from './utils/datadog';
import { getIdentifier, DATADOG_INDEXING_WAIT_5_MIN_MS } from '../config';

const runtimes = ['node', 'python', 'java', 'dotnet'] as const;
type Runtime = typeof runtimes[number];

const identifier = getIdentifier();
const stackName = `integ-${identifier}-otlp`;

describe('OTLP Integration Tests', () => {
  let telemetry: Record<string, DatadogTelemetry>;

  beforeAll(async () => {
    // Build function configs for all runtimes plus response validation and gRPC
    const functions: FunctionConfig[] = [
      ...runtimes.map(runtime => ({
        functionName: `${stackName}-${runtime}-lambda`,
        runtime,
      })),
      {
        functionName: `${stackName}-response-validation-lambda`,
        runtime: 'responseValidation',
      },
      {
        functionName: `${stackName}-node-grpc-lambda`,
        runtime: 'nodeGrpc',
      },
    ];

    console.log('Invoking all OTLP Lambda functions...');

    // Invoke all OTLP functions and collect telemetry
    telemetry = await invokeAndCollectTelemetry(functions, 1, 1, 0, {}, DATADOG_INDEXING_WAIT_5_MIN_MS);

    console.log('All OTLP Lambda invocations and data fetching completed');
  }, 700000);

  describe.each(runtimes)('%s Runtime', (runtime) => {
    const getResult = () => telemetry[runtime]?.threads[0]?.[0];

    it('should invoke Lambda successfully', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should send at least one trace to Datadog', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.traces?.length).toBeGreaterThan(0);
    });

    it('should have spans in the trace', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      expect(trace.spans.length).toBeGreaterThan(0);
    });
  });

  describe('OTLP Response Validation', () => {
    const getResult = () => telemetry['responseValidation']?.threads[0]?.[0];

    it('should invoke response validation Lambda successfully', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should have JSON encoded span in Datadog', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const allSpans = result.traces?.flatMap(t => t.spans) || [];
      const hasJsonSpan = allSpans.some(s =>
        s.attributes?.resource_name === 'test-span-json' && s.attributes?.custom?.encoding === 'json'
      );
      expect(hasJsonSpan).toBe(true);
    });

    it('should have Protobuf encoded span in Datadog', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const allSpans = result.traces?.flatMap(t => t.spans) || [];
      const hasProtobufSpan = allSpans.some(s =>
        s.attributes?.resource_name === 'test-span-protobuf' && s.attributes?.custom?.encoding === 'protobuf'
      );
      expect(hasProtobufSpan).toBe(true);
    });
  });

  describe('OTLP gRPC Protocol', () => {
    const getResult = () => results['nodeGrpc']?.[0]?.[0];

    it('should invoke gRPC Lambda successfully', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.statusCode).toBe(200);
    });

    it('should send traces via gRPC to Datadog', () => {
      const result = getResult();
      expect(result).toBeDefined();
      expect(result.traces?.length).toEqual(1);
    });

    it('should have gRPC handler span with correct attributes', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const allSpans = result.traces?.flatMap(t => t.spans) || [];
      const hasGrpcSpan = allSpans.some(s =>
        s.attributes?.resource_name === 'grpc-handler' && s.attributes?.custom?.protocol === 'grpc'
      );
      expect(hasGrpcSpan).toBe(true);
    });

    it('should have spans in the trace', () => {
      const result = getResult();
      expect(result).toBeDefined();
      const trace = result.traces![0];
      expect(trace.spans.length).toBeGreaterThan(0);
    });
  });
});
