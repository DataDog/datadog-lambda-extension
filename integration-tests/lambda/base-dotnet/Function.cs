using Amazon.Lambda.Core;
using System.Collections.Generic;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace Function
{
    public class Handler
    {
        public Dictionary<string, object> FunctionHandler(Dictionary<string, object> input, ILambdaContext context)
        {
            context.Logger.LogLine("Hello world!");
            return new Dictionary<string, object>
            {
                { "statusCode", 200 }
            };
        }
    }
}
