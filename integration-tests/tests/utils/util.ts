import { DatadogLog, DatadogTrace, getTraces, getLogs } from "./datadog";
import { invokeLambda } from "./lambda";

export interface LambdaInvocationDatadogData {
    requestId: string;
    statusCode: number;
    payload: any;
    traces?: DatadogTrace[];
    logs?: DatadogLog[];
}

export async function invokeLambdaAndGetDatadogData(functionName: string, payload: any = {}, coldStart: boolean = false): Promise<LambdaInvocationDatadogData> {
    const result = await invokeLambda(functionName, payload, coldStart, false);

    await new Promise(resolve => setTimeout(resolve, 600000));

    const traces = await getTraces(functionName, result.requestId);
    const logs = await getLogs(functionName, result.requestId);

    const lambdaInvocationData: LambdaInvocationDatadogData = {
        requestId: result.requestId,
        statusCode: result.statusCode,
        payload: result.payload,
        traces,
        logs,
    };

    return lambdaInvocationData;

}