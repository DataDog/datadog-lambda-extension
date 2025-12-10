using Amazon.Lambda.Core;
using OpenTelemetry;
using OpenTelemetry.Exporter;
using OpenTelemetry.Resources;
using OpenTelemetry.Trace;
using System;
using System.Collections.Generic;
using System.Diagnostics;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace Function;

public class Handler
{
    private static readonly TracerProvider tracerProvider;
    private static readonly ActivitySource activitySource;

    static Handler()
    {
        try
        {
            string endpoint = Environment.GetEnvironmentVariable("OTEL_EXPORTER_OTLP_ENDPOINT") ?? "http://localhost:4318";
            string serviceName = Environment.GetEnvironmentVariable("OTEL_SERVICE_NAME") ?? "otlp-dotnet-lambda";

            activitySource = new ActivitySource(serviceName);

            tracerProvider = Sdk.CreateTracerProviderBuilder()
                .AddSource(serviceName)
                .SetResourceBuilder(ResourceBuilder.CreateDefault().AddService(serviceName))
                .AddOtlpExporter(options =>
                {
                    options.Endpoint = new Uri(endpoint + "/v1/traces");
                    options.Protocol = OtlpExportProtocol.HttpProtobuf;
                })
                .Build();
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[OTLP] Error initializing OpenTelemetry: {ex.Message}");
            Console.WriteLine($"[OTLP] Stack trace: {ex.StackTrace}");
        }
    }

    public Dictionary<string, object> FunctionHandler(Dictionary<string, object> input, ILambdaContext context)
    {
        using (var activity = activitySource.StartActivity("handler", ActivityKind.Server))
        {
            activity?.SetTag("request_id", context.AwsRequestId);
            activity?.SetTag("http.status_code", 200);
        }
        var flushResult = tracerProvider.ForceFlush(30000);
        return new Dictionary<string, object>
        {
            ["statusCode"] = 200,
            ["body"] = "{\"message\": \"Success\"}"
        };
    }
}
