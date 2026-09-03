import {
  CloudWatchLogsClient,
  FilterLogEventsCommand,
} from '@aws-sdk/client-cloudwatch-logs';

const logsClient = new CloudWatchLogsClient({ region: 'us-east-1' });

/**
 * Returns the messages of all log events in a Lambda's CloudWatch log group
 * matching `filterPattern` within [startTime, endTime] (epoch ms). The Datadog
 * extension's logs land here too, so use a quoted literal
 * (e.g. '"payload size after enrichment"') to read extension-emitted lines.
 */
export async function filterLogMessages(
  functionName: string,
  filterPattern: string,
  startTime: number,
  endTime: number,
): Promise<string[]> {
  const logGroupName = `/aws/lambda/${functionName}`;
  const messages: string[] = [];
  let nextToken: string | undefined;

  do {
    const response = await logsClient.send(
      new FilterLogEventsCommand({
        logGroupName,
        filterPattern,
        startTime,
        endTime,
        nextToken,
      }),
    );
    for (const event of response.events ?? []) {
      if (event.message) {
        messages.push(event.message);
      }
    }
    nextToken = response.nextToken;
  } while (nextToken);

  return messages;
}

/**
 * Returns the number of log events in a Lambda's CloudWatch log group within
 * [startTime, endTime] (epoch ms), regardless of content. Used to distinguish
 * a silent function from a functioning one whose lines have not become
 * searchable yet.
 */
export async function countLogEvents(
  functionName: string,
  startTime: number,
  endTime: number,
): Promise<number> {
  const logGroupName = `/aws/lambda/${functionName}`;
  let count = 0;
  let nextToken: string | undefined;

  do {
    const response = await logsClient.send(
      new FilterLogEventsCommand({
        logGroupName,
        startTime,
        endTime,
        nextToken,
      }),
    );
    count += response.events?.length ?? 0;
    nextToken = response.nextToken;
  } while (nextToken);

  return count;
}
