exports.handler = async () => {
    const response = {
        statusCode: 200,
        body: JSON.stringify('Hello from lambda !'),
    };
    return response;
};
