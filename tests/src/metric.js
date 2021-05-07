const { sendDistributionMetric } = require("datadog-lambda-js");

let invocationCount = 0;
    
exports.metricTest = (_, __, callback) => {
    sendDistributionMetric("serverless.lambda-extension.integration-test.count", invocationCount);
    invocationCount += 1;
    callback(null, {
        statusCode: 200, 
        body: "ok"
    });
};