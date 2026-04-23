using Amazon.Lambda.Core;
using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Threading;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace Function
{
    public class Handler
    {
        public Dictionary<string, object> FunctionHandler(JsonElement input, ILambdaContext context)
        {
            context.Logger.LogLine("Hello world!");

            var sleepMsStr = Environment.GetEnvironmentVariable("SLEEP_MS");
            if (!string.IsNullOrEmpty(sleepMsStr) && int.TryParse(sleepMsStr, out int sleepMs) && sleepMs > 0)
            {
                context.Logger.LogLine($"Sleeping for {sleepMs}ms");
                Thread.Sleep(sleepMs);
            }

            var body = new Dictionary<string, object>
            {
                { "message", "Success" },
                { "requestId", context.AwsRequestId }
            };

            return new Dictionary<string, object>
            {
                { "statusCode", 200 },
                { "body", JsonSerializer.Serialize(body) }
            };
        }
    }
}
