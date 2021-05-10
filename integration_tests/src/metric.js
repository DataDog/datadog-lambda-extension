const { datadog, sendDistributionMetric } = require("datadog-lambda-js");

let invocationCount = 0;

async function myHandler(event, context) {
  sendDistributionMetric("serverless.lambda-extension.integration-test.count", invocationCount);
  invocationCount += 1;
  return {
    statusCode: 200,
    body: 'ok'
  };
}

module.exports.enhancedMetricTest = datadog(myHandler);