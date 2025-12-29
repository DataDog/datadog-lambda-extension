exports.handler = async (event, context) => {
  console.log('Hello World!');
  return {
    statusCode: 200,
    body: JSON.stringify({
      message: 'Success from Lambda Managed Instance',
      requestId: context.requestId
    })
  };
};
