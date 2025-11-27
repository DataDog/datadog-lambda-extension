exports.handler = async (event, context) => {
  console.log('Hello world!');
  return {
    statusCode: 200,
    body: 'Hello world!',
  };
};
