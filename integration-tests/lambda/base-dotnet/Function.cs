using Amazon.Lambda.Core;
using System.Collections.Generic;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace BaseDotnet
{
    public class Function
    {
        public Dictionary<string, object> FunctionHandler(Dictionary<string, object> input, ILambdaContext context)
        {
            Console.WriteLine("Hello world!");

            return new Dictionary<string, object>
            {
                { "statusCode", 200 }
            };
        }
    }
}
