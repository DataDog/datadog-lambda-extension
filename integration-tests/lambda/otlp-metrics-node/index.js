// dd-trace (DD_METRICS_OTEL_ENABLED) registers the global MeterProvider and exports to
// localhost:4318/v1/metrics every OTEL_METRIC_EXPORT_INTERVAL ms.
const { metrics } = require('@opentelemetry/api');

const COUNTER_NAME = 'integration.otlp_metrics.requests';
const COUNTER_INCREMENT = 5;
const EXPORT_WAIT_MS = 3000;

exports.handler = async (event, context) => {
  metrics.getMeter('otlp-metrics-node').createCounter(COUNTER_NAME).add(COUNTER_INCREMENT);

  // The function freezes after it returns, so wait for a periodic export during the invocation.
  await new Promise(resolve => setTimeout(resolve, EXPORT_WAIT_MS));

  return {
    statusCode: 200,
    body: JSON.stringify({ message: 'Success', requestId: context.awsRequestId }),
  };
};
