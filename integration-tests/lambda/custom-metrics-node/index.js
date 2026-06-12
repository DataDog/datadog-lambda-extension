const dgram = require("dgram");

exports.handler = async (event, context) => {
  console.log("Sending custom DogStatsD metric");

  await new Promise((resolve, reject) => {
    const client = dgram.createSocket("udp4");
    const message = Buffer.from("custom.exclude_tags_test:1|c");
    client.send(message, 8125, "127.0.0.1", (err) => {
      client.close();
      if (err) reject(err);
      else resolve();
    });
  });

  // Give the extension a moment to receive the UDP packet
  await new Promise((resolve) => setTimeout(resolve, 100));

  console.log("Custom metric sent");

  return {
    statusCode: 200,
    body: JSON.stringify({
      message: "Success",
      requestId: context.awsRequestId,
    }),
  };
};
