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
  trace_id: string;
  span_id: string;
  service: string;
  name: string;
  resource: string;
  start: number;
  duration: number;
  meta?: Record<string, string>;
  metrics?: Record<string, number>;
}

export interface DatadogLog {
  id: string;
  attributes: {
    timestamp: string;
    message: string;
    service?: string;
    tags?: string[];
    [key: string]: any;
  };
}

export interface DatadogMetric {
  metric: string;
  points: Array<[number, number]>;
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
  const fromTime = now - (15 * 60 * 1000); // 15 minutes ago
  const toTime = now;
  try {
    // Build query with service name and optional request ID
    let query = `service:${serviceName}`;
    if (requestId) {
      query += ` @request_id:${requestId}`;
    }

    // Use the correct v2 spans list API format
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
            limit: 1000,
          },
          sort: '-timestamp',
        },
      },
    });

    const spans = response.data.data || [];

    console.log(`API returned ${spans.length} spans`);

    // Group spans by trace_id to reconstruct traces
    const traceMap = new Map<string, DatadogSpan[]>();

    for (const spanData of spans) {
      const attrs = spanData.attributes || {};
      const span: DatadogSpan = {
        trace_id: attrs['trace_id'] || attrs.trace_id || '',
        span_id: attrs['span_id'] || attrs.span_id || '',
        service: attrs['service'] || attrs.service || '',
        name: attrs['name'] || attrs['operation_name'] || attrs.name || '',
        resource: attrs['resource_name'] || attrs.resource || '',
        start: attrs['start'] || attrs.start || 0,
        duration: attrs['duration'] || attrs.duration || 0,
        meta: attrs['tags'] || attrs.meta || {},
        metrics: attrs['metrics'] || {},
      };

      const traceId = span.trace_id;
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
  const fromTime = now - (15 * 60 * 1000); // 15 minutes ago
  const toTime = now;
  try {
    // Build query with service name and optional request ID
    let query = `service:${serviceName}`;
    if (requestId) {
      query += ` @lambda.request_id:${requestId}`;
    }

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

    return response.data.data || [];
  } catch (error: any) {
    console.error('Error searching logs:', error.response?.data || error.message);
    throw error;
  }
}

/**
 * Query metrics from Datadog
 * @param query - Metrics query (e.g., "avg:aws.lambda.duration{function_name:my-function}")
 * @param from - Start time in seconds since epoch
 * @param to - End time in seconds since epoch
 */
export async function queryMetrics(
  serviceName: string,
): Promise<DatadogMetric[]> {
  const now = Date.now();
  const fromTime = now - (15 * 60 * 1000); // 15 minutes ago
  const toTime = now;
  try {
    const response = await datadogClient.post('/api/v2/metrics/query', {
      params: {
        data: {
          type: 'query_request',
          attributes: {
            query: `service:${serviceName}`,
            from: new Date(fromTime).toISOString(),
            to: new Date(toTime).toISOString(),
          },
        },
      },
    });

    return response.data.series || [];
  } catch (error: any) {
    console.error('Error querying metrics:', error.response?.data || error.message);
    throw error;
  }
}

/**
 * Wait for data to appear in Datadog with retries
 */
export async function waitForData<T>(
  fetchFn: () => Promise<T[]>,
  options: {
    timeout?: number;
    interval?: number;
    minCount?: number;
  } = {}
): Promise<T[]> {
  const timeout = options.timeout || 180000; // 3 minutes default
  const interval = options.interval || 10000; // 10 seconds default
  const minCount = options.minCount || 1;

  const startTime = Date.now();
  let attempts = 0;

  while (Date.now() - startTime < timeout) {
    attempts++;
    console.log(`Attempt ${attempts}: Checking for data in Datadog...`);

    try {
      const data = await fetchFn();
      if (data.length >= minCount) {
        console.log(`Found ${data.length} items in Datadog`);
        return data;
      }
      console.log(`Found ${data.length} items, waiting for at least ${minCount}...`);
    } catch (error) {
      console.log(`Error fetching data (attempt ${attempts}):`, error);
    }

    await new Promise(resolve => setTimeout(resolve, interval));
  }

  throw new Error(`Timeout waiting for data in Datadog after ${timeout}ms`);
}
