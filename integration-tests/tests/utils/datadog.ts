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
  threads: InvocationTracesLogs[][];  // [thread][invocation]
  metrics: EnhancedMetrics;
}

export interface InvocationTracesLogs {
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

export const ENHANCED_METRICS_CONFIG = {
  duration: [
    'aws.lambda.enhanced.runtime_duration',
    'aws.lambda.enhanced.billed_duration',
    'aws.lambda.enhanced.duration',
    'aws.lambda.enhanced.post_runtime_duration',
    'aws.lambda.enhanced.init_duration',
  ],
} as const;

export type MetricCategory = keyof typeof ENHANCED_METRICS_CONFIG;

export type EnhancedMetrics = {
  [K in MetricCategory]: Record<string, MetricPoint[]>;
};

export interface MetricPoint {
  timestamp: number;
  value: number;
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

export async function getInvocationTracesLogsByRequestId(functionName: string, requestId: string): Promise<InvocationTracesLogs> {
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
  const fromTime = now - (1 * 60 * 60 * 1000);
  const toTime = now;
  const serviceNameLower = serviceName.toLowerCase();
  const query = `service:${serviceNameLower} @request_id:${requestId}`;

  try {
    console.log(`Searching for traces: ${query}`);

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

    const traceIds = new Set<string>();
    for (const spanData of initialSpans) {
      const traceId = spanData.attributes?.trace_id;
      if (traceId) {
        traceIds.add(traceId);
      }
    }

    console.log(`Found ${traceIds.size} unique trace(s)`);

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
 * Search for logs in Datadog
 * @param serviceName - Datadog service name
 * @param requestId - AWS Lambda request ID to filter by
 */
export async function getLogs(
  serviceName: string,
  requestId: string,
): Promise<DatadogLog[]> {
  const now = Date.now();
  const fromTime = now - (2 * 60 * 60 * 1000);
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

/**
 * Fetch all enhanced metrics for a function based on config
 */
export async function getEnhancedMetrics(
  functionName: string,
  fromTime: number,
  toTime: number
): Promise<EnhancedMetrics> {
  const result: Partial<EnhancedMetrics> = {};

  const categoryPromises = Object.entries(ENHANCED_METRICS_CONFIG).map(
    async ([category, metricNames]) => {
      const categoryMetrics = await fetchMetricCategory(
        metricNames as readonly string[],
        functionName,
        fromTime,
        toTime
      );
      return { category, metrics: categoryMetrics };
    }
  );

  const categoryResults = await Promise.all(categoryPromises);

  for (const { category, metrics } of categoryResults) {
    result[category as MetricCategory] = metrics;
  }

  return result as EnhancedMetrics;
}

/**
 * Fetch all metrics in a category in parallel
 */
async function fetchMetricCategory(
  metricNames: readonly string[],
  functionName: string,
  fromTime: number,
  toTime: number
): Promise<Record<string, MetricPoint[]>> {
  const promises = metricNames.map(async (metricName) => {
    const points = await getMetrics(metricName, functionName, fromTime, toTime);
    const shortName = metricName.split('.').pop()!;
    return { shortName, points };
  });

  const results = await Promise.all(promises);

  const metrics: Record<string, MetricPoint[]> = {};
  for (const { shortName, points } of results) {
    metrics[shortName] = points;
  }

  return metrics;
}

/**
 * Query Datadog Metrics API v1 for a specific metric.
 * Requires the DD_API_KEY to have 'timeseries_query' scope.
 */
async function getMetrics(
  metricName: string,
  functionName: string,
  fromTime: number,
  toTime: number
): Promise<MetricPoint[]> {
  const baseFunctionName = getServiceName(functionName).toLowerCase();
  const query = `avg:${metricName}{functionname:${baseFunctionName}}`;

  console.log(`Querying metrics: ${query}`);

  const response = await datadogClient.get('/api/v1/query', {
    params: {
      query,
      from: Math.floor(fromTime / 1000),
      to: Math.floor(toTime / 1000),
    },
  });

  const series = response.data.series || [];
  console.log(`Found ${series.length} series for ${metricName}`);

  if (series.length === 0) {
    return [];
  }

  return (series[0].pointlist || []).map((p: [number, number]) => ({
    timestamp: p[0],
    value: p[1],
  }));
}
