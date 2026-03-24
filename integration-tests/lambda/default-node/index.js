exports.handler = async (event, context) => {
  console.log('Hello world!');

  const sleepMs = parseInt(process.env.SLEEP_MS || '0', 10);
  if (sleepMs > 0) {
    console.log(`Sleeping for ${sleepMs}ms`);
    await new Promise(resolve => setTimeout(resolve, sleepMs));
  }

  return {
    statusCode: 200,
    body: JSON.stringify({
      message: 'Success',
      requestId: context.awsRequestId
    })
  };
};
