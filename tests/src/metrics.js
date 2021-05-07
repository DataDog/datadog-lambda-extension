const _ = require("dd-trace").init({
    flushInterval: 0,
    logLevel: "debug",
});

const { datadog, sendDistributionMetric } = require("datadog-lambda-js");

let invocationCount = 0;
    
exports.metricTest = datadog(
    (_, _, _) => {
        sendDistributionMetric("serverless.lambda-extension.integration-test.count", invocationCount);
        invocationCount += 1;
        return {
            statusCode: 200
        };
    },
    { debugLogging: true, mergeDatadogXrayTraces: false }
);
    