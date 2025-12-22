const { Resource } = require('@opentelemetry/resources');
const { BasicTracerProvider, InMemorySpanExporter } = require('@opentelemetry/sdk-trace-base');
const { createExportTraceServiceRequest } = require('@opentelemetry/otlp-transformer');
const { trace, SpanKind, SpanStatusCode } = require('@opentelemetry/api');

const root = require('@opentelemetry/otlp-transformer/build/esm/generated/root');
const ExportTraceServiceRequest = root.opentelemetry.proto.collector.trace.v1.ExportTraceServiceRequest;
const ExportTraceServiceResponse = root.opentelemetry.proto.collector.trace.v1.ExportTraceServiceResponse;

exports.handler = async (event, context) => {
  console.log('Starting OTLP response validation...');
  const result = await validateOtlpResponse();
  return {
    statusCode: result.success ? 200 : 500,
    body: JSON.stringify(result, null, 2),
  };
};

async function validateOtlpResponse() {
  try {
    const requestBuffer = createOtlpTraceRequest();
    const response = await fetch('http://localhost:4318/v1/traces', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/x-protobuf',
      },
      body: requestBuffer,
    });

    if (response.status !== 200) {
      return {
        success: false,
        error: `Expected status 200, got ${response.status}`,
        statusCode: response.status,
      };
    }

    const responseBuffer = Buffer.from(await response.arrayBuffer());

    const contentType = response.headers.get('content-type');
    if (contentType !== 'application/x-protobuf') {
      return {
        success: false,
        error: `Expected Content-Type 'application/x-protobuf', got '${contentType}'`,
        statusCode: response.status,
        contentType,
      };
    }

    let decodedResponse;
    try {
      decodedResponse = ExportTraceServiceResponse.decode(responseBuffer);
    } catch (decodeError) {
      return {
        success: false,
        error: `Failed to decode response as ExportTraceServiceResponse: ${decodeError.message}`,
        statusCode: response.status,
        contentType,
        decodeError: decodeError.message,
      };
    }

    if (decodedResponse.partialSuccess) {
      const rejectedSpans = decodedResponse.partialSuccess.rejectedSpans || 0;
      if (rejectedSpans > 0) {
        return {
          success: false,
          error: `Unexpected partial success with ${rejectedSpans} rejected spans`,
          statusCode: response.status,
          contentType,
          partialSuccess: decodedResponse.partialSuccess,
        };
      }
    }

    return {
      success: true,
      statusCode: response.status,
      contentType,
      response: decodedResponse,
      message: 'OTLP response is properly formatted and decodable',
    };

  } catch (error) {
    console.error('Validation error:', error);
    return {
      success: false,
      error: `Unexpected error: ${error.message}`,
      stack: error.stack,
    };
  }
}

function createOtlpTraceRequest() {
  const resource = new Resource({
    'service.name': process.env.DD_SERVICE,
  });

  const provider = new BasicTracerProvider({ resource });
  const exporter = new InMemorySpanExporter();
  provider.register();

  const tracer = provider.getTracer('validation-tracer', '1.0.0');
  const span = tracer.startSpan('test-span', {
    kind: SpanKind.INTERNAL,
    attributes: {
      'test': 'validation',
    },
  });
  span.setStatus({ code: SpanStatusCode.OK });
  span.end();

  provider.forceFlush();

  const finishedSpans = exporter.getFinishedSpans();
  const otlpRequest = createExportTraceServiceRequest(finishedSpans, {
    useHex: true,
    useLongBits: false,
  });

  const message = ExportTraceServiceRequest.create(otlpRequest);
  const buffer = ExportTraceServiceRequest.encode(message).finish();
  return Buffer.from(buffer);
}
