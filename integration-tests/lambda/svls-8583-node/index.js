'use strict';

// Handler for SVLS-8583 integration tests.
// Returns { Status: process.env.RETURN_STATUS } so the test can configure each function
// to return a specific durable execution status.
// When the Lambda event contains a DurableExecutionArn, the datadog-lambda-js wrapper
// reads result.Status and sets aws_lambda.durable_function.execution_status on the span.
exports.handler = async (event, context) => {
  const returnStatus = process.env.RETURN_STATUS || 'SUCCEEDED';
  console.log(`Durable function handler: returning Status=${returnStatus}`);
  return {
    Status: returnStatus,
    statusCode: 200,
  };
};
