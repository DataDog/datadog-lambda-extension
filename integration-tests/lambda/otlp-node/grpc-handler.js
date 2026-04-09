const { NodeTracerProvider } = require('@opentelemetry/sdk-trace-node');
const { BatchSpanProcessor } = require('@opentelemetry/sdk-trace-base');
const { OTLPTraceExporter } = require('@opentelemetry/exporter-trace-otlp-grpc');
const { Resource } = require('@opentelemetry/resources');
const { ATTR_SERVICE_NAME } = require('@opentelemetry/semantic-conventions');
const api = require('@opentelemetry/api');

const resource = new Resource({
  [ATTR_SERVICE_NAME]: process.env.OTEL_SERVICE_NAME || 'otlp-grpc-node-lambda',
});

const provider = new NodeTracerProvider({ resource });
const processor = new BatchSpanProcessor(
  new OTLPTraceExporter({
    url: process.env.OTEL_EXPORTER_OTLP_ENDPOINT,
  })
);
provider.addSpanProcessor(processor);
provider.register();

api.trace.setGlobalTracerProvider(provider);

exports.handler = async (event, context) => {
    const tracer = api.trace.getTracer('otlp-grpc-node-lambda');

    await tracer.startActiveSpan('grpc-handler', async (span) => {
        try {
            span.setAttribute('request_id', context.awsRequestId);
            span.setAttribute('protocol', 'grpc');
            span.setAttribute('http.status_code', 200);
        } catch (error) {
            span.recordException(error);
            span.setStatus({ code: api.SpanStatusCode.ERROR, message: error.message });
            throw error;
        } finally {
            span.end();
        }
    });
    await provider.forceFlush();

    return {
        statusCode: 200,
        body: JSON.stringify({ message: 'Success via gRPC' })
    };
};
