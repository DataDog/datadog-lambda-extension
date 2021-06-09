const tracer = require("dd-trace").init({
  flushInterval: 0,
  logLevel: "debug",
});

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

async function myTimeoutHandler(event, context) {
  sendDistributionMetric("serverless.lambda-extension.integration-test.count", invocationCount);
  await new Promise(r => setTimeout(r, 30*1000)); //30 sec to be sure 
  invocationCount += 1;
  return {
    statusCode: 200,
    body: 'ok'
  };
}

module.exports.enhancedMetricTest = datadog(myHandler);
module.exports.noEnhancedMetricTest = datadog(myHandler);
module.exports.timeoutMetricTest = datadog(myTimeoutHandler);