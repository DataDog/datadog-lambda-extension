import axios, { AxiosInstance } from 'axios';

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
 * Search for traces in Datadog using the v2 API
 * @param serviceName - Datadog service name
 * @param requestId - Optional AWS Lambda request ID to filter by
 */
export async function getTraces(
  serviceName: string,
  requestId?: string,
): Promise<DatadogTrace[]> {
  const now = Date.now();
  const fromTime = now - (2 * 60 * 60 * 1000); // 2 hours ago
  const toTime = now;
  try {
    // Convert service name to lowercase as Datadog stores it that way
    const serviceNameLower = serviceName.toLowerCase();

    // Build query with service name and optional request ID
    let query = `service:${serviceNameLower}`;
    if (requestId) {
      query += ` @request_id:${requestId}`;
    }

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
  } catch (error: any) {
    console.error('Error searching traces:', error.response?.data || error.message);
    throw error;
  }
}


/**
 * Search for logs in Datadog
 * @param serviceName - Datadog service name
 * @param requestId - Optional AWS Lambda request ID to filter by
 */
export async function getLogs(
  serviceName: string,
  requestId?: string,
): Promise<DatadogLog[]> {
  const now = Date.now();
  const fromTime = now - (2 * 60 * 60 * 1000); // 2 hours ago
  const toTime = now;
  try {
    let query = `service:${serviceName}`;
    if (requestId) {
      query += ` @lambda.request_id:${requestId}`;
    }

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
  } catch (error: any) {
    console.error('Error searching logs:', error.response?.data || error.message);
    throw error;
  }
}