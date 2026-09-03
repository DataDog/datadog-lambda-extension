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
 * `maxPages` can bound scans used only for diagnostics.
 */
export async function filterLogMessages(
  functionName: string,
  filterPattern: string,
  startTime: number,
  endTime: number,
  maxPages: number = Number.POSITIVE_INFINITY,
): Promise<string[]> {
  const logGroupName = `/aws/lambda/${functionName}`;
  const messages: string[] = [];
  let pages = 0;
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
    pages += 1;
  } while (nextToken && pages < maxPages);

  if (nextToken) {
    console.log(`filterLogMessages: stopped after ${maxPages} pages, result is incomplete`);
  }

  return messages;
}

/**
 * Returns the number of log events in a Lambda's CloudWatch log group within
 * [startTime, endTime] (epoch ms), regardless of content. Used to distinguish
 * a silent function from a functioning one whose lines have not become
 * searchable yet.
 *
 * This scan is unfiltered, so a debug-level log group can span many pages;
 * `maxPages` bounds it so a diagnostic can never outlast the test it is
 * diagnosing. The result is a lower bound once the cap is hit.
 */
export async function countLogEvents(
  functionName: string,
  startTime: number,
  endTime: number,
  maxPages: number = 20,
): Promise<number> {
  const logGroupName = `/aws/lambda/${functionName}`;
  let count = 0;
  let pages = 0;
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
    pages += 1;
  } while (nextToken && pages < maxPages);

  if (nextToken) {
    console.log(`countLogEvents: stopped after ${maxPages} pages, ${count} is a lower bound`);
  }

  return count;
}
