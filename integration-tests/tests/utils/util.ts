import { DatadogLog, DatadogTrace, getTraces, getLogs } from "./datadog";
import { invokeLambda } from "./lambda";

export interface LambdaInvocationDatadogData {
    requestId: string;
    statusCode: number;
    payload: any;
    traces?: DatadogTrace[];
    logs?: DatadogLog[];
}

export async function invokeLambdaAndGetDatadogData(functionName: string, payload: any = {}, coldStart: boolean = false, useTailLogs: boolean = true): Promise<LambdaInvocationDatadogData> {
    const result = await invokeLambda(functionName, payload, coldStart, useTailLogs);

    console.log('Waiting for logs and traces to be indexed in Datadog...');
    await new Promise(resolve => setTimeout(resolve, 60_000));

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