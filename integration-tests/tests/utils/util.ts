import { DatadogLog, DatadogTrace, getTraces, getLogs } from "./datadog";
import { invokeLambda, forceColdStart } from "./lambda";
import { DEFAULT_DATADOG_INDEXING_WAIT_MS } from '../../config';

export interface LambdaInvocationDatadogData {
    requestId: string;
    statusCode: number;
    traces?: DatadogTrace[];
    logs?: DatadogLog[];
}

export async function invokeLambdaAndGetDatadogData(
    functionName: string,
    payload: any = {},
    coldStart: boolean = false,
    datadogIndexingWaitMs: number = DEFAULT_DATADOG_INDEXING_WAIT_MS
): Promise<LambdaInvocationDatadogData> {
    if (coldStart) {
        await forceColdStart(functionName);
    }
    const result = await invokeLambda(functionName, payload);

    console.log(`Waiting ${datadogIndexingWaitMs / 1000}s for logs and traces to be indexed in Datadog...`);
    await new Promise(resolve => setTimeout(resolve, datadogIndexingWaitMs));

    // Strip alias suffix (e.g., ":snapstart") for Datadog queries since service name doesn't include it
    const baseFunctionName = functionName.split(':')[0];

    const traces = await getTraces(baseFunctionName, result.requestId);
    // Use the Lambda execution request ID (from tail logs) for log filtering when available.
    // For durable functions, tail logs are unsupported and executionRequestId is undefined,
    // so getLogs falls back to a service-only query and returns all recent logs for that function.
    const logs = await getLogs(baseFunctionName, result.executionRequestId);

    return {
        requestId: result.requestId,
        statusCode: result.statusCode,
        traces,
        logs,
    };
}
