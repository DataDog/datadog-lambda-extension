import { hasDataStreamsLatency } from "./utils/datadog";
import { forceColdStart, invokeLambda } from "./utils/lambda";
import { getIdentifier, DEFAULT_DATADOG_INDEXING_WAIT_MS } from "../config";

const identifier = getIdentifier();
const stackName = `integ-${identifier}-dsm`;
const functionName = `${stackName}-sqs-consumer`;

// Queue name the consume edge tag (topic:) is derived from — the last ':'
// segment of the SQS eventSourceARN.
const QUEUE_NAME = "dsm-integ-queue";

// A known-good producer DSM pathway context (non-zero parent hash). The
// extension reads this from messageAttributes._datadog and records a consume
// checkpoint parented onto it.
const PRODUCER_PATHWAY_CTX_B64 = "Z7CzXmXArPrE58Cfj2LI2cOfj2I=";

/**
 * Synthetic SQS event. When the Java consumer is invoked with this payload,
 * dd-trace-java forwards it to /lambda/start-invocation, the extension infers
 * the SQS trigger, reads the _datadog carrier, and records a DSM consume
 * checkpoint with edge tags [direction:in, topic:<queue>, type:sqs].
 */
function syntheticSqsEvent() {
  // A complete SQS record. The extension deserializes Records[0] strictly, so
  // all of messageId/receiptHandle/attributes/md5OfBody/awsRegion/etc. must be
  // present or trigger inference is skipped ("missing field `messageId`").
  return {
    Records: [
      {
        messageId: "059f36b4-87a3-44ab-83d2-661975830a7d",
        receiptHandle: "AQEBwJnKyrHigUMZj6rYigCgxlaS3SLy0a",
        body: "hello from dsm integration test",
        attributes: {
          ApproximateReceiveCount: "1",
          SentTimestamp: "1545082649183",
          SenderId: "AIDAIENQZJOLO23YVJ4VO",
          ApproximateFirstReceiveTimestamp: "1545082649185",
        },
        messageAttributes: {
          _datadog: {
            dataType: "String",
            stringValue: JSON.stringify({
              "dd-pathway-ctx-base64": PRODUCER_PATHWAY_CTX_B64,
            }),
          },
        },
        md5OfBody: "e4e68fb7bd0e697a0ae8f1bb342846b3",
        eventSource: "aws:sqs",
        eventSourceARN: `arn:aws:sqs:us-east-1:123456789012:${QUEUE_NAME}`,
        awsRegion: "us-east-1",
      },
    ],
  };
}

describe("DSM extension-side consume checkpoint", () => {
  let fromTime: number;
  let toTime: number;

  beforeAll(async () => {
    await forceColdStart(functionName);

    // Back up the window so the 10s DSM bucket (which the backend aligns to a
    // boundary that may precede the invocation) falls inside the query range.
    fromTime = Date.now() - 60_000;

    await invokeLambda(functionName, syntheticSqsEvent());

    // DSM buckets in 10s windows and the backend processes asynchronously, so
    // use the long indexing wait like the other metric-based suites.
    await new Promise((resolve) =>
      setTimeout(resolve, DEFAULT_DATADOG_INDEXING_WAIT_MS),
    );

    toTime = Date.now();

    console.log("DSM consumer invoked and indexing wait complete");
  }, 900000);

  it("creates a DSM node for the consumer service", async () => {
    // `service` is always a tag on data_streams.latency (per the DSM team), so
    // its presence proves the extension created a consume node for this service.
    const exists = await hasDataStreamsLatency(functionName, [], fromTime, toTime);
    expect(exists).toBe(true);
  });

  // The following two assertions verify the consume edge tags actually landed,
  // not just that *some* DSM node exists. They depend on `type` / `topic` being
  // surfaced as tags on data_streams.latency. Verify on first run; if those tags
  // are not present, the service-only assertion above remains the gate.
  it("tags the consume node with type:sqs", async () => {
    const exists = await hasDataStreamsLatency(
      functionName,
      ["type:sqs"],
      fromTime,
      toTime,
    );
    expect(exists).toBe(true);
  });

  it("tags the consume node with the source queue as topic", async () => {
    const exists = await hasDataStreamsLatency(
      functionName,
      [`topic:${QUEUE_NAME}`],
      fromTime,
      toTime,
    );
    expect(exists).toBe(true);
  });
});
