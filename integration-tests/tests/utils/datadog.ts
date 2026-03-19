import axios, { AxiosInstance, AxiosError } from 'axios';

const DD_API_KEY = process.env.DD_API_KEY;
const DD_APP_KEY = process.env.DD_APP_KEY;
const DD_SITE = process.env.DD_SITE || 'datadoghq.com';

if (!DD_API_KEY || !DD_APP_KEY) {
  console.warn('Warning: DD_API_KEY and DD_APP_KEY environment variables are not set. Datadog API tests will fail.');
}

const datadogClient: AxiosInstance = axios.create({
  baseURL: `https://api.${DD_SITE}`,
  headers: {
    'DD-API-KEY': DD_API_KEY,
    'DD-APPLICATION-KEY': DD_APP_KEY,
  },
  timeout: 30000,
});

function formatDatadogError(error: unknown, query: string): string {
  if (error instanceof AxiosError && error.response) {
    const status = error.response.status;
    const statusText = error.response.statusText || '';
    let message = `HTTP ${status} ${statusText}`.trim();

    // Include rate limit info for 429 errors
    if (status === 429) {
      const retryAfter = error.response.headers['x-ratelimit-reset'] || error.response.headers['retry-after'];
      if (retryAfter) {
        message += ` (retry-after: ${retryAfter}s)`;
      }
      const remaining = error.response.headers['x-ratelimit-remaining'];
      if (remaining !== undefined) {
        message += ` (remaining: ${remaining})`;
      }
    }

    // Include response body if available
    const responseData = error.response.data;
    if (responseData) {
      const bodyStr = typeof responseData === 'string' ? responseData : JSON.stringify(responseData);
      if (bodyStr && bodyStr !== '{}') {
        message += ` - ${bodyStr}`;
      }
    }

    return `Error (query: '${query}'): ${message}`;
  }

  if (error instanceof Error) {
    return `Error (query: '${query}'): ${error.message}`;
  }

  return `Error (query: '${query}'): ${String(error)}`;
}

export interface DatadogTelemetry {
  requestId: string;
  statusCode?: number;
  traces?: DatadogTrace[];
  logs?: DatadogLog[];
}

export interface DatadogTrace {
  trace_id: string;
  spans: DatadogSpan[];
}

export interface DatadogSpan {
  attributes: Record<string, any>;
}

export interface DatadogLog {
  attributes: Record<string, any>;
  message: string;
  status: string;
  timestamp: string;
  tags: string[];
}

/**
 * Extracts the base service name from a function name by stripping any
 * version qualifier (:N) or alias qualifier (:alias)
 */
function getServiceName(functionName: string): string {
  const colonIndex = functionName.lastIndexOf(':');
  if (colonIndex === -1) {
    return functionName;
  }
  return functionName.substring(0, colonIndex);
}

export async function getDatadogTelemetryByRequestId(functionName: string, requestId: string): Promise<DatadogTelemetry> {
  const serviceName = getServiceName(functionName);
  const traces = await getTraces(serviceName, requestId);
  const logs = await getLogs(serviceName, requestId);
  return { requestId, traces, logs };
}

/**
 * Search for traces in Datadog using the v2 API
 * @param serviceName - Datadog service name
 * @param requestId - AWS Lambda request ID to filter by
 */
export async function getTraces(
  serviceName: string,
  requestId: string,
): Promise<DatadogTrace[]> {
  const now = Date.now();
  const fromTime = now - (1 * 60 * 60 * 1000); // 1 hour ago
  const toTime = now;
  // Convert service name to lowercase as Datadog stores it that way
  const serviceNameLower = serviceName.toLowerCase();
  const query = `service:${serviceNameLower} @request_id:${requestId}`;

  try {
    console.log(`Searching for traces: ${query}`);

    // First, find spans matching the request_id to get trace IDs
    const initialResponse = await datadogClient.post('/api/v2/spans/events/search', {
      data: {
        type: 'search_request',
        attributes: {
          filter: {
            query: query,
            from: new Date(fromTime).toISOString(),
            to: new Date(toTime).toISOString(),
          },
          page: {
            limit: 1000,
          },
          sort: '-timestamp',
        },
      },
    });

    const initialSpans = initialResponse.data.data || [];
    console.log(`Found ${initialSpans.length} initial span(s)`);

    // Extract unique trace IDs
    const traceIds = new Set<string>();
    for (const spanData of initialSpans) {
      const traceId = spanData.attributes?.trace_id;
      if (traceId) {
        traceIds.add(traceId);
      }
    }

    console.log(`Found ${traceIds.size} unique trace(s)`);

    // Now fetch all spans for each trace ID
    const allSpans: any[] = [];
    for (const traceId of traceIds) {
      const traceResponse = await datadogClient.post('/api/v2/spans/events/search', {
        data: {
          type: 'search_request',
          attributes: {
            filter: {
              query: `trace_id:${traceId}`,
              from: new Date(fromTime).toISOString(),
              to: new Date(toTime).toISOString(),
            },
            page: {
              limit: 1000,
            },
          },
        },
      });
      const traceSpans = traceResponse.data.data || [];
      console.log(`Trace ${traceId}: ${traceSpans.length} spans`);
      allSpans.push(...traceSpans);
    }

    // Group spans by trace_id to reconstruct traces
    const traceMap = new Map<string, DatadogSpan[]>();

    for (const spanData of allSpans) {
      const attrs = spanData.attributes || {};

      const span: DatadogSpan = {
        attributes: attrs,
      };

      const traceId = attrs['trace_id'] || attrs.trace_id || '';
      if (traceId && !traceMap.has(traceId)) {
        traceMap.set(traceId, []);
      }
      if (traceId) {
        traceMap.get(traceId)!.push(span);
      }
    }

    // Convert map to array of traces
    const traces: DatadogTrace[] = [];
    for (const [traceId, spans] of traceMap.entries()) {
      traces.push({
        trace_id: traceId,
        spans: spans,
      });
    }

    return traces;
  } catch (error: unknown) {
    console.error(`Error searching traces: ${formatDatadogError(error, query)}`);
    throw error;
  }
}

/**
 * Search for ALL spans from a service (without request_id filter)
 * Used for diagnostic purposes to see what spans exist
 */
export async function searchAllSpans(
  serviceName: string,
  limit: number = 50,
): Promise<any[]> {
  const now = Date.now();
  const fromTime = now - (1 * 60 * 60 * 1000); // 1 hour ago
  const toTime = now;
  const serviceNameLower = serviceName.toLowerCase();
  const query = `service:${serviceNameLower}`;

  try {
    console.log(`Searching for all spans: ${query}`);

    const response = await datadogClient.post('/api/v2/spans/events/search', {
      data: {
        type: 'search_request',
        attributes: {
          filter: {
            query: query,
            from: new Date(fromTime).toISOString(),
            to: new Date(toTime).toISOString(),
          },
          page: {
            limit: limit,
          },
          sort: '-timestamp',
        },
      },
    });

    const spans = response.data.data || [];
    console.log(`Found ${spans.length} total span(s) for service`);

    // Log unique operation names
    const opNames = new Set<string>();
    for (const span of spans) {
      const opName = span.attributes?.operation_name || 'unknown';
      opNames.add(opName);
    }
    console.log(`Unique operation names: ${Array.from(opNames).join(', ')}`);

    // Log details of each span including request_id
    console.log('\n=== All Spans Detail ===');
    for (const span of spans) {
      const opName = span.attributes?.operation_name || 'unknown';
      const resource = span.attributes?.resource_name || 'unknown';
      const requestId = span.attributes?.custom?.request_id || span.attributes?.request_id || 'NONE';
      const traceId = span.attributes?.trace_id || 'unknown';
      const start = span.attributes?.start || 0;
      const adjusted = span.attributes?.custom?._dd?.snapstart_adjusted === 'true' ||
                       span.attributes?.custom?.['_dd.snapstart_adjusted'] === 'true';

      console.log(`  ${opName}: resource=${resource}, trace_id=${traceId}, request_id=${requestId}${adjusted ? ' [ADJUSTED]' : ''}`);
    }

    return spans;
  } catch (error: unknown) {
    console.error(`Error searching all spans: ${formatDatadogError(error, query)}`);
    return [];
  }
}

/**
 * Search for logs in Datadog
 * @param serviceName - Datadog service name
 * @param requestId - AWS Lambda request ID to filter by
 */
export async function getLogs(
  serviceName: string,
  requestId: string,
): Promise<DatadogLog[]> {
  const now = Date.now();
  const fromTime = now - (2 * 60 * 60 * 1000); // 2 hours ago
  const toTime = now;
  const query = `service:${serviceName} @lambda.request_id:${requestId}`;

  try {
    console.log(`Searching for logs: ${query}`);

    const response = await datadogClient.post('/api/v2/logs/events/search', {
      filter: {
        query: query,
        from: new Date(fromTime).toISOString(),
        to: new Date(toTime).toISOString(),
      },
      page: {
        limit: 1000,
      },
    });

    const rawLogs = response.data.data || [];
    console.log(`Found ${rawLogs.length} log(s)`);

    // Transform raw logs to DatadogLog format
    const logs: DatadogLog[] = rawLogs.map((logData: any) => {
      const attrs = logData.attributes || {};
      return {
        attributes: attrs,
        message: attrs.message || '',
        status: attrs.status || '',
        timestamp: attrs.timestamp || '',
        tags: attrs.tags || [],
      };
    });

    return logs;
  } catch (error: unknown) {
    console.error(`Error searching logs: ${formatDatadogError(error, query)}`);
    throw error;
  }
}
