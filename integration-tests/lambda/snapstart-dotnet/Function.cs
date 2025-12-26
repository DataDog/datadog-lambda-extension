using Amazon.Lambda.Core;
using System.Collections.Generic;
using System.Threading.Tasks;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace Function
{
    public class SnapstartHandler
    {
        public async Task<Dictionary<string, object>> FunctionHandler(Dictionary<string, object> input, ILambdaContext context)
        {
            // Wait 10 seconds to guarantee concurrent execution overlap
            await Task.Delay(10000);

            return new Dictionary<string, object>
            {
                { "statusCode", 200 },
                { "body", "Snapstart .NET function executed" }
            };
        }
    }
}
