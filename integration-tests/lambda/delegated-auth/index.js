/**
 * Lambda handler for delegated authentication integration tests.
 *
 * This is a simple handler that logs a message and returns success.
 * The extension will handle API key resolution (delegated auth or fallback).
 */
exports.handler = async (event, context) => {
  console.log('Delegated auth test function invoked');
  console.log('Request ID:', context.awsRequestId);

  // Log environment info (without secrets)
  const orgUuid = process.env.DD_ORG_UUID;
  const hasApiKey = !!process.env.DD_API_KEY;
  const hasApiKeySecretArn = !!process.env.DD_API_KEY_SECRET_ARN;

  console.log('DD_ORG_UUID configured:', orgUuid ? 'yes' : 'no');
  console.log('DD_API_KEY configured:', hasApiKey ? 'yes' : 'no');
  console.log('DD_API_KEY_SECRET_ARN configured:', hasApiKeySecretArn ? 'yes' : 'no');

  // Simple work to generate some telemetry
  const startTime = Date.now();
  await new Promise(resolve => setTimeout(resolve, 100));
  const duration = Date.now() - startTime;

  console.log(`Work completed in ${duration}ms`);

  return {
    statusCode: 200,
    body: JSON.stringify({
      message: 'Delegated auth test completed',
      requestId: context.awsRequestId,
      duration: duration,
    }),
  };
};
