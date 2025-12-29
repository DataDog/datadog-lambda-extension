using Amazon.Lambda.Core;
using System.Collections.Generic;
using System.Threading.Tasks;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace Function
{
    public class Handler
    {
        public async Task<Dictionary<string, object>> FunctionHandler(Dictionary<string, object> input, ILambdaContext context)
        {
            context.Logger.LogLine("Hello World!");
            return new Dictionary<string, object>
            {
                { "statusCode", 200 }
            };
        }
    }
}
