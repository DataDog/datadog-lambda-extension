const { NodeTracerProvider } = require('@opentelemetry/sdk-trace-node');
const { BatchSpanProcessor } = require('@opentelemetry/sdk-trace-base');
const { OTLPTraceExporter } = require('@opentelemetry/exporter-trace-otlp-proto');
const { Resource } = require('@opentelemetry/resources');
const { ATTR_SERVICE_NAME } = require('@opentelemetry/semantic-conventions');
const api = require('@opentelemetry/api');

const resource = new Resource({
  [ATTR_SERVICE_NAME]: process.env.OTEL_SERVICE_NAME || 'otlp-node-lambda',
});

const provider = new NodeTracerProvider({ resource });
const processor = new BatchSpanProcessor(
  new OTLPTraceExporter({
    url: process.env.OTEL_EXPORTER_OTLP_ENDPOINT + '/v1/traces',
  })
);
provider.addSpanProcessor(processor);
provider.register();

api.trace.setGlobalTracerProvider(provider);

exports.handler = async (event, context) => {
    const tracer = api.trace.getTracer('otlp-node-lambda');

    await tracer.startActiveSpan('handler', async (span) => {
        try {
            span.setAttribute('request_id', context.awsRequestId);
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
        body: JSON.stringify({ message: 'Success' })
    };
};
