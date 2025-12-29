import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from '../config';
import { publishVersion, waitForSnapStartReady } from './utils/lambda';

describe('Snapstart Integration Tests', () => {
  const identifier = getIdentifier();

  const results = {
    javaInvocation1: null as LambdaInvocationDatadogData | null,
    javaInvocation2: null as LambdaInvocationDatadogData | null,
    dotnetInvocation1: null as LambdaInvocationDatadogData | null,
    dotnetInvocation2: null as LambdaInvocationDatadogData | null,
    pythonInvocation1: null as LambdaInvocationDatadogData | null,
    pythonInvocation2: null as LambdaInvocationDatadogData | null,
  };

  beforeAll(async () => {
    const javaBaseFunctionName = `integ-${identifier}-snapstart-java-lambda`;
    const dotnetBaseFunctionName = `integ-${identifier}-snapstart-dotnet-lambda`;
    const pythonBaseFunctionName = `integ-${identifier}-snapstart-python-lambda`;

    console.log('Publishing new versions for all Snapstart Lambda functions...');
    const javaVersion = await publishVersion(javaBaseFunctionName);
    const dotnetVersion  = await publishVersion(dotnetBaseFunctionName);
    const pythonVersion = await publishVersion(pythonBaseFunctionName);
    console.log(`All Lambda versions published successfully - Java: ${javaVersion}, .NET: ${dotnetVersion}, Python: ${pythonVersion}`);

    console.log('Waiting for SnapStart optimization to complete on all versions...');
    await waitForSnapStartReady(javaBaseFunctionName, javaVersion);
    await waitForSnapStartReady(dotnetBaseFunctionName, dotnetVersion);
    await waitForSnapStartReady(pythonBaseFunctionName, pythonVersion);
    console.log('All SnapStart versions are ready');

    console.log('Invoking all Snapstart Lambda functions concurrently...');
    const javaFunctionName = `${javaBaseFunctionName}:${javaVersion}`;
    const dotnetFunctionName = `${dotnetBaseFunctionName}:${dotnetVersion}`;
    const pythonFunctionName = `${pythonBaseFunctionName}:${pythonVersion}`;
    const invocationResults = await Promise.all([
      invokeLambdaAndGetDatadogData(javaFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(javaFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(dotnetFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(dotnetFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(pythonFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(pythonFunctionName, {}, false),
    ]);

    results.javaInvocation1 = invocationResults[0];
    results.javaInvocation2 = invocationResults[1];
    results.dotnetInvocation1 = invocationResults[2];
    results.dotnetInvocation2 = invocationResults[3];
    results.pythonInvocation1 = invocationResults[4];
    results.pythonInvocation2 = invocationResults[5];

    console.log('All Snapstart Lambda invocations and data fetching completed');
  }, 900000);

  describe('Java Runtime with SnapStart', () => {
    testSnapStartInvocation(() => results.javaInvocation1!);
    testSnapStartInvocation(() => results.javaInvocation2!);
    testTraceIsolation(() => results.javaInvocation1!, () => results.javaInvocation2!);
  });

  describe('.NET Runtime with SnapStart', () => {
    testSnapStartInvocation(() => results.dotnetInvocation1!);
    testSnapStartInvocation(() => results.dotnetInvocation2!);
    testTraceIsolation(() => results.dotnetInvocation1!, () => results.dotnetInvocation2!);
  });

  // SVLS-5988 - Doesn't completely work as expected.
  // describe('Python Runtime with SnapStart', () => {
  //   testSnapStartInvocation(() => results.pythonInvocation1!);
  //   testSnapStartInvocation(() => results.pythonInvocation2!);
  //   testTraceIsolation(() => results.pythonInvocation1!, () => results.pythonInvocation2!);
  // });

});

function testSnapStartInvocation(getInvocation: () => LambdaInvocationDatadogData) {
  it('should invoke successfully', () => {
    const invocation = getInvocation();
    expect(invocation.statusCode).toBe(200);
  });

  it('should send one trace to Datadog', () => {
    const invocation = getInvocation();
    expect(invocation.traces?.length).toEqual(1);
  });

  it('should have aws.lambda span with Snapstart properties', () => {
    const invocation = getInvocation();
    const trace = invocation.traces![0];
    const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
    );
    expect(awsLambdaSpan).toBeDefined();
    expect(awsLambdaSpan?.attributes.custom.init_type).toBe('snap-start');
  });

  it('should have aws.lambda.snapstart_restore span', () => {
    const invocation = getInvocation();
    const trace = invocation.traces![0];
    const restoreSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda.snapstart_restore'
    );
    expect(restoreSpan).toBeDefined();
    expect(restoreSpan).toMatchObject({
      attributes: {
        operation_name: 'aws.lambda.snapstart_restore',
      }
    });

  });

  it('should NOT have aws.lambda.cold_start span (replaced by restore span)', () => {
    const invocation = getInvocation();
    const trace = invocation.traces![0];
    const coldStartSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda.cold_start'
    );
    expect(coldStartSpan).toBeUndefined();
  });
}

function testTraceIsolation(getInvocation1: () => LambdaInvocationDatadogData, getInvocation2: () => LambdaInvocationDatadogData) {
  describe('Trace Isolation Between Concurrent Invocations', () => {
    it('should have different trace IDs for each invocation', () => {
      const invocation1 = getInvocation1();
      const invocation2 = getInvocation2();
      const trace1 = invocation1.traces![0];
      const trace2 = invocation2.traces![0];

      expect(trace1.trace_id).not.toEqual(trace2.trace_id);
    });
  });
}

