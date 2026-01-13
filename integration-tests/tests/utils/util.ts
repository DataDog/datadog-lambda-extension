import { DatadogLog, DatadogTrace, getTraces, getLogs } from "./datadog";
import { invokeLambda } from "./lambda";

// Default wait time for Datadog to index logs and traces after Lambda invocation
const DEFAULT_DATADOG_INDEXING_WAIT_MS = 60 * 1000; // 60 seconds

// Extended wait time for tests that need more time (e.g., OTLP tests)
export const DATADOG_INDEXING_WAIT_5_MIN_MS = 5 * 60 * 1000; // 5 minutes

export interface LambdaInvocationDatadogData {
    requestId: string;
    statusCode: number;
    payload: any;
    traces?: DatadogTrace[];
    logs?: DatadogLog[];
}

export async function invokeLambdaAndGetDatadogData(
    functionName: string,
    payload: any = {},
    coldStart: boolean = false,
    useTailLogs: boolean = true,
    datadogIndexingWaitMs: number = DEFAULT_DATADOG_INDEXING_WAIT_MS
): Promise<LambdaInvocationDatadogData> {
    const result = await invokeLambda(functionName, payload, coldStart, useTailLogs);

    console.log(`Waiting ${datadogIndexingWaitMs / 1000}s for logs and traces to be indexed in Datadog...`);
    await new Promise(resolve => setTimeout(resolve, datadogIndexingWaitMs));

    // Strip alias suffix (e.g., ":snapstart") for Datadog queries since service name doesn't include it
    const baseFunctionName = functionName.split(':')[0];

    const traces = await getTraces(baseFunctionName, result.requestId);
    const logs = await getLogs(baseFunctionName, result.requestId);

    const lambdaInvocationData: LambdaInvocationDatadogData = {
        requestId: result.requestId,
        statusCode: result.statusCode,
        payload: result.payload,
        traces,
        logs,
    };

    return lambdaInvocationData;
}