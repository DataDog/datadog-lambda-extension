package example;

import com.amazonaws.services.lambda.runtime.Context;
import com.amazonaws.services.lambda.runtime.RequestHandler;
import io.opentelemetry.api.GlobalOpenTelemetry;
import io.opentelemetry.api.trace.Span;
import io.opentelemetry.api.trace.Tracer;
import io.opentelemetry.context.Scope;
import io.opentelemetry.exporter.otlp.http.trace.OtlpHttpSpanExporter;
import io.opentelemetry.sdk.OpenTelemetrySdk;
import io.opentelemetry.sdk.resources.Resource;
import io.opentelemetry.sdk.trace.SdkTracerProvider;
import io.opentelemetry.sdk.trace.export.BatchSpanProcessor;

import java.util.HashMap;
import java.util.Map;

public class Handler implements RequestHandler<Map<String, Object>, Map<String, Object>> {
    private static final Tracer tracer;
    private static final OpenTelemetrySdk sdk;

    static {
        String endpoint = System.getenv().getOrDefault("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318");
        String serviceName = System.getenv().getOrDefault("OTEL_SERVICE_NAME", "otlp-java-lambda");

        Resource resource = Resource.getDefault().toBuilder()
                .put("service.name", serviceName)
                .build();

        OtlpHttpSpanExporter spanExporter = OtlpHttpSpanExporter.builder()
                .setEndpoint(endpoint + "/v1/traces")
                .build();

        SdkTracerProvider tracerProvider = SdkTracerProvider.builder()
                .addSpanProcessor(BatchSpanProcessor.builder(spanExporter).build())
                .setResource(resource)
                .build();

        sdk = OpenTelemetrySdk.builder()
                .setTracerProvider(tracerProvider)
                .build();

        GlobalOpenTelemetry.set(sdk);
        tracer = sdk.getTracer("otlp-java-lambda");
    }

    @Override
    public Map<String, Object> handleRequest(Map<String, Object> event, Context context) {
        Span span = tracer.spanBuilder("handler").startSpan();

        try (Scope scope = span.makeCurrent()) {
            String requestId = context.getAwsRequestId();
            span.setAttribute("request_id", requestId);
            span.setAttribute("http.status_code", 200);

            Map<String, Object> response = new HashMap<>();
            response.put("statusCode", 200);
            response.put("body", "{\"message\": \"Success\"}");
            return response;
        } finally {
            span.end();
            sdk.getSdkTracerProvider().forceFlush().join(30, java.util.concurrent.TimeUnit.SECONDS);
        }
    }
}
