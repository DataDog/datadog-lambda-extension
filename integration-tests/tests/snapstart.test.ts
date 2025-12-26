import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';
import { publishVersion, waitForSnapStartReady } from './utils/lambda';

describe('Snapstart Integration Tests', () => {
  const identifier = getIdentifier();

  const java = {
    invocation1: null as LambdaInvocationDatadogData | null,
    invocation2: null as LambdaInvocationDatadogData | null,
  };
  const dotnet = {
    invocation1: null as LambdaInvocationDatadogData | null,
    invocation2: null as LambdaInvocationDatadogData | null,
  };
  const python = {
    invocation1: null as LambdaInvocationDatadogData | null,
    invocation2: null as LambdaInvocationDatadogData | null,
  }

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
    const results = await Promise.all([
      invokeLambdaAndGetDatadogData(javaFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(javaFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(dotnetFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(dotnetFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(pythonFunctionName, {}, false),
      invokeLambdaAndGetDatadogData(pythonFunctionName, {}, false),
    ]);
    [java.invocation1, java.invocation2, dotnet.invocation1, dotnet.invocation2, python.invocation1, python.invocation2] = results;

  }, 900000);

  describe('Java Runtime with SnapStart', () => {
    testSnapStartInvocation(java.invocation1!)
    testSnapStartInvocation(java.invocation2!)
    testTraceIsolation(java.invocation1!, java.invocation2!)
  })

  describe('.NET Runtime with SnapStart', () => {
    testSnapStartInvocation(dotnet.invocation1!)
    testSnapStartInvocation(dotnet.invocation2!)
    testTraceIsolation(dotnet.invocation1!, dotnet.invocation2!)
  })

  describe('Python Runtime with SnapStart', () => {
    testSnapStartInvocation(python.invocation1!)
    testSnapStartInvocation(python.invocation2!)
    // SVLS-5988 - Doesn't completely work as expected.
    // testTraceIsolation(python.invocation1!, python.invocation2!)
  })

});

function testSnapStartInvocation(invocation: LambdaInvocationDatadogData) {
  it('should invoke successfully', () => {
    expect(invocation.statusCode).toBe(200);
  });

  it('should send one trace to Datadog', () => {
    expect(invocation.traces?.length).toEqual(1);
  });

  it('should have aws.lambda span with Snapstart properties', () => {
    const trace = invocation.traces![0];
    const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
    );
    expect(awsLambdaSpan).toBeDefined();
    expect(awsLambdaSpan?.attributes.custom.init_type).toBe('snap-start');
  });

  it('should have aws.lambda.snapstart_restore span', () => {
    const trace = invocation.traces![0];
    const awsLambdaSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda'
    );
    expect(awsLambdaSpan?.attributes.custom.cold_start).toBe('true');

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
    const trace = invocation.traces![0];
    const coldStartSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda.cold_start'
    );
    expect(coldStartSpan).toBeUndefined();
  });
}

function testWarmInvocation(invocation: LambdaInvocationDatadogData) {
  it('should invoke successfully', () => {
    expect(invocation.statusCode).toBe(200);
  });

  it('should send one trace to Datadog', () => {
    expect(invocation.traces?.length).toEqual(1);
  });

  it('should NOT have aws.lambda.snapstart_restore span', () => {
    const trace = invocation.traces![0];
    const restoreSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda.snapstart_restore'
    );
    expect(restoreSpan).toBeUndefined();
  });

  it('should NOT have aws.lambda.cold_start span', () => {
    const trace = invocation.traces![0];
    const coldStartSpan = trace.spans.find((span: any) =>
        span.attributes.operation_name === 'aws.lambda.cold_start'
    );
    expect(coldStartSpan).toBeUndefined();
  });
}

function testTraceIsolation(invocation1: LambdaInvocationDatadogData, invocation2: LambdaInvocationDatadogData) {
  describe('Trace Isolation Between Concurrent Invocations', () => {
    it('should have different trace IDs for each invocation', () => {
      const trace1 = invocation1.traces![0];
      const trace2 = invocation2.traces![0];

      expect(trace1.trace_id).not.toEqual(trace2.trace_id);
    });

    it('should NOT have traces linked together', () => {
      const trace1 = invocation1.traces![0];
      const trace2 = invocation2.traces![0];

      const awsLambdaSpan1 = trace1.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );
      const awsLambdaSpan2 = trace2.spans.find((span: any) =>
          span.attributes.operation_name === 'aws.lambda'
      );

      expect(awsLambdaSpan1).toBeDefined();
      expect(awsLambdaSpan2).toBeDefined();

      if (awsLambdaSpan1 && awsLambdaSpan2) {
        expect(awsLambdaSpan1.attributes.parent_id).not.toEqual(awsLambdaSpan2.attributes.span_id);
        expect(awsLambdaSpan2.attributes.parent_id).not.toEqual(awsLambdaSpan1.attributes.span_id);
      }
    });
  });
}

