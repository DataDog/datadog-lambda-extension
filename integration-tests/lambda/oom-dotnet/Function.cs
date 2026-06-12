using Amazon.Lambda.Core;
using System.Collections.Generic;
using System.Text.Json;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]

namespace Function
{
    /// <summary>
    /// OOM reproducer for .NET. Allocates and retains 10 MB byte arrays in a list
    /// until the CLR throws System.OutOfMemoryException. Bottlecap's runtime-specific
    /// log-line detection matches "OutOfMemoryException".
    /// </summary>
    public class Handler
    {
        public Dictionary<string, object> FunctionHandler(JsonElement input, ILambdaContext context)
        {
            var data = new List<byte[]>();
            while (true)
            {
                data.Add(new byte[10 * 1024 * 1024]);
            }
        }
    }
}
