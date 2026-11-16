exports.handler = async (event, context) => {
  const mode = event && event.mode;

  if (mode === 'timeout') {
    // Exceeds the 10 s function timeout.
    await new Promise(resolve => setTimeout(resolve, 30000));
  } else if (mode === 'crash') {
    process.exit(1);
  }

  return {
    statusCode: 200,
    body: JSON.stringify({
      message: 'Success',
      requestId: context.awsRequestId
    })
  };
};
