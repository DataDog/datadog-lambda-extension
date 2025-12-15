from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.resources import Resource
from opentelemetry.semconv.resource import ResourceAttributes
import os

# Initialize OpenTelemetry SDK with Protobuf/HTTP exporter
resource = Resource(attributes={
    ResourceAttributes.SERVICE_NAME: os.environ.get('OTEL_SERVICE_NAME', 'otlp-python-lambda')
})

provider = TracerProvider(resource=resource)
processor = BatchSpanProcessor(
    OTLPSpanExporter(
        endpoint=os.environ.get('OTEL_EXPORTER_OTLP_ENDPOINT', 'http://localhost:4318') + '/v1/traces'
    )
)
provider.add_span_processor(processor)
trace.set_tracer_provider(provider)
tracer = trace.get_tracer(__name__)


def handler(event, context):
    with tracer.start_as_current_span("handler") as span:
        span.set_attribute("request_id", context.aws_request_id)
        span.set_attribute("http.status_code", 200)

    # Force flush to ensure traces are sent before Lambda freezes
    trace.get_tracer_provider().force_flush(timeout_millis=30000)

    return {
        'statusCode': 200,
        'body': '{"message": "Success"}'
    }
