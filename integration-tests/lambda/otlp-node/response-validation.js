const { NodeTracerProvider } = require('@opentelemetry/sdk-trace-node');
const { SimpleSpanProcessor } = require('@opentelemetry/sdk-trace-base');
const { OTLPTraceExporter: OTLPTraceExporterHttp } = require('@opentelemetry/exporter-trace-otlp-http');
const { OTLPTraceExporter: OTLPTraceExporterProto } = require('@opentelemetry/exporter-trace-otlp-proto');
const { Resource } = require('@opentelemetry/resources');
const { ATTR_SERVICE_NAME } = require('@opentelemetry/semantic-conventions');

exports.handler = async (event, context) => {
  const serviceName = context.functionName;
  const requestId = context.awsRequestId;
  const endpoint = process.env.OTEL_EXPORTER_OTLP_ENDPOINT || 'http://localhost:4318';

  const jsonProvider = new NodeTracerProvider({
    resource: new Resource({ [ATTR_SERVICE_NAME]: serviceName }),
  });
  const jsonExporter = new OTLPTraceExporterHttp({
    url: endpoint + '/v1/traces',
    headers: { 'Content-Type': 'application/json' },
  });
  jsonProvider.addSpanProcessor(new SimpleSpanProcessor(jsonExporter));
  jsonProvider.register();

  const jsonSpan = jsonProvider.getTracer('json-test').startSpan('test-span-json');
  jsonSpan.setAttribute('request_id', requestId);
  jsonSpan.setAttribute('encoding', 'json');
  jsonSpan.end();

  await jsonProvider.forceFlush();
  await jsonProvider.shutdown();

  const protoProvider = new NodeTracerProvider({
    resource: new Resource({ [ATTR_SERVICE_NAME]: serviceName }),
  });
  const protoExporter = new OTLPTraceExporterProto({
    url: endpoint + '/v1/traces',
  });
  protoProvider.addSpanProcessor(new SimpleSpanProcessor(protoExporter));

  const protoSpan = protoProvider.getTracer('proto-test').startSpan('test-span-protobuf');
  protoSpan.setAttribute('request_id', requestId);
  protoSpan.setAttribute('encoding', 'protobuf');
  protoSpan.end();

  await protoProvider.forceFlush();
  await protoProvider.shutdown();

  return { statusCode: 200, body: JSON.stringify({ message: 'Success' }) };
};
