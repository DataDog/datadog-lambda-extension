using Amazon.Lambda.Core;
using System.Collections.Generic;
using System.Text.Json;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace Function
{
    public class Handler
    {
        public Dictionary<string, object> FunctionHandler(JsonElement input, ILambdaContext context)
        {
            context.Logger.LogLine("Hello world!");

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
