using System;
using System.IO;
using System.Net;
using Amazon.Lambda.Core;

[assembly: LambdaSerializer(typeof(Amazon.Lambda.Serialization.SystemTextJson.DefaultLambdaJsonSerializer))]
namespace Serverless
{
    public class Function
    {
       public APIGatewayProxyResponse Handler(APIGatewayProxyRequest apigProxyEvent)
           {
               Console.WriteLine($"Processing request data for request {apigProxyEvent.RequestContext.RequestId}.");
               Console.WriteLine($"Body size = {apigProxyEvent.Body.Length}.");
               var headerNames = string.Join(", ", apigProxyEvent.Headers.Keys);
               Console.WriteLine($"Specified headers = {headerNames}.");

               return new APIGatewayProxyResponse
               {
                   Body = apigProxyEvent.Body,
                   StatusCode = 200,
               };
           }
    }
}
