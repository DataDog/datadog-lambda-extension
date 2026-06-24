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

const MAX_RETRY_WAIT_MS = 5 * 60 * 1000;
const DEFAULT_RETRY_AFTER_MS = 5000;
const MAX_SINGLE_WAIT_MS = 60 * 1000;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function parseRetryAfterMs(error: AxiosError): number {
  const headers = error.response?.headers ?? {};
  const raw = headers['x-ratelimit-reset'] ?? headers['retry-after'];
  const seconds = raw !== undefined ? Number(raw) : NaN;
  const ms = Number.isFinite(seconds) && seconds > 0 ? seconds * 1000 : DEFAULT_RETRY_AFTER_MS;
  return Math.min(ms, MAX_SINGLE_WAIT_MS);
}

async function requestWithRetry<T>(fn: () => Promise<T>, query: string): Promise<T> {
  let waited = 0;
  let attempt = 0;
  // eslint-disable-next-line no-constant-condition
  while (true) {
    try {
      return await fn();
    } catch (error: unknown) {
      const is429 = error instanceof AxiosError && error.response?.status === 429;
      if (!is429) {
        throw error;
      }
      const jitter = Math.floor(Math.random() * 1000);
      const wait = parseRetryAfterMs(error as AxiosError) + jitter;
      if (waited + wait > MAX_RETRY_WAIT_MS) {
        throw error;
      }
      attempt += 1;
      waited += wait;
      console.warn(
        `Datadog API 429 for '${query}'; retrying in ${wait}ms (attempt ${attempt}, total waited ${waited}ms)`,
      );
      await sleep(wait);
    }
  }
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

export const DURATION_METRICS = [
  'aws.lambda.enhanced.runtime_duration',
  'aws.lambda.enhanced.billed_duration',
  'aws.lambda.enhanced.duration',
  'aws.lambda.enhanced.post_runtime_duration',
  'aws.lambda.enhanced.init_duration',
];

export const OUT_OF_MEMORY_METRIC = 'aws.lambda.enhanced.out_of_memory';

export type EnhancedMetrics = Record<string, MetricPoint[]>;

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

    const initialResponse = await requestWithRetry(() => datadogClient.post('/api/v2/spans/events/search', {
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
    }), query);

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
      const traceQuery = `trace_id:${traceId}`;
      const traceResponse = await requestWithRetry(() => datadogClient.post('/api/v2/spans/events/search', {
        data: {
          type: 'search_request',
          attributes: {
            filter: {
              query: traceQuery,
              from: new Date(fromTime).toISOString(),
              to: new Date(toTime).toISOString(),
            },
            page: {
              limit: 1000,
            },
          },
        },
      }), traceQuery);
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

    const response = await requestWithRetry(() => datadogClient.post('/api/v2/logs/events/search', {
      filter: {
        query: query,
        from: new Date(fromTime).toISOString(),
        to: new Date(toTime).toISOString(),
      },
      page: {
        limit: 1000,
      },
    }), query);

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

export async function getEnhancedMetrics(
  functionName: string,
  fromTime: number,
  toTime: number
): Promise<EnhancedMetrics> {
  const promises = DURATION_METRICS.map(async (metricName) => {
    const points = await getMetrics(metricName, functionName, fromTime, toTime);
    return { metricName, points };
  });

  const results = await Promise.all(promises);

  const metrics: EnhancedMetrics = {};
  for (const { metricName, points } of results) {
    metrics[metricName] = points;
  }

  return metrics;
}

/**
 * Returns the total emission count of a counter / distribution enhanced metric
 * for a single function over the given window, by summing all data-point
 * values returned by Datadog. Used by oom.test.ts to assert that
 * `aws.lambda.enhanced.out_of_memory` increments exactly once per invocation —
 * verifying the per-Context `oom_emitted` dedup flag introduced for #1237.
 */
export async function getMetricCount(
  metricName: string,
  functionName: string,
  fromTime: number,
  toTime: number,
): Promise<number> {
  const baseFunctionName = getServiceName(functionName).toLowerCase();
  const query = `sum:${metricName}{functionname:${baseFunctionName}}.as_count()`;

  console.log(`Querying metric count: ${query}`);

  const response = await requestWithRetry(() => datadogClient.get('/api/v1/query', {
    params: {
      query,
      from: Math.floor(fromTime / 1000),
      to: Math.floor(toTime / 1000),
    },
  }), query);

  const series = response.data.series || [];
  if (series.length === 0) {
    return 0;
  }

  const pointlist: [number, number][] = series[0].pointlist || [];
  return pointlist.reduce((acc, [, value]) => acc + (value || 0), 0);
}

async function getMetrics(
  metricName: string,
  functionName: string,
  fromTime: number,
  toTime: number
): Promise<MetricPoint[]> {
  const baseFunctionName = getServiceName(functionName).toLowerCase();
  const query = `avg:${metricName}{functionname:${baseFunctionName}}`;

  console.log(`Querying metrics: ${query}`);

  const response = await requestWithRetry(() => datadogClient.get('/api/v1/query', {
    params: {
      query,
      from: Math.floor(fromTime / 1000),
      to: Math.floor(toTime / 1000),
    },
  }), query);

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

/**
 * Query a custom metric filtered by function name and an additional tag filter.
 * Use tagFilter like "function_arn:*" to check if a tag exists on the metric.
 * Returns true if the query returns data points, false otherwise.
 */
export async function hasMetricWithTag(
  metricName: string,
  functionName: string,
  tagFilter: string,
  fromTime: number,
  toTime: number,
): Promise<boolean> {
  const baseFunctionName = getServiceName(functionName).toLowerCase();
  const query = `avg:${metricName}{functionname:${baseFunctionName},${tagFilter}}`;

  console.log(`Querying metric with tag filter: ${query}`);

  const response = await requestWithRetry(() => datadogClient.get('/api/v1/query', {
    params: {
      query,
      from: Math.floor(fromTime / 1000),
      to: Math.floor(toTime / 1000),
    },
  }), query);

  const series = response.data.series || [];
  const hasData = series.some((s: any) => Array.isArray(s.pointlist) && s.pointlist.length > 0);
  console.log(`Tag filter query returned ${series.length} series, hasData=${hasData}`);
  return hasData;
}
