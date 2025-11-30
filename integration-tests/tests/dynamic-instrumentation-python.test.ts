import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from './utils/config';

describe('Dynamic Instrumentation Python Lambda Integration Test', () => {
  const DI_FUNCTION_NAME = `integ-${getIdentifier()}-dynamic-instrumentation-python-lambda`;
  const PROBE_ID = 'test_probe_process_data';
  let result: LambdaInvocationDatadogData;

  beforeAll(async () => {
    console.log(`Invoking Lambda function: ${DI_FUNCTION_NAME}`);
    const probePayload = {
      probes: [
        {
          id: PROBE_ID,
          type: 'LOG_PROBE',
          where: {
            sourceFile: 'lambda_function.py',
            lines: ['18']  // Line 18: return statement (after multiplication completes)
          },
          captureSnapshot: true,
          capture: {
            maxReferenceDepth: 2
          },
          tags: []
        }
      ],
      payload: {
        value: 42
      }
    };

    result = await invokeLambdaAndGetDatadogData(DI_FUNCTION_NAME, probePayload, true);
    console.log(`Found ${result.logs?.length} logs`);
  }, 700000); // 11.6 minute timeout

  it('should invoke Python Lambda successfully', () => {
    expect(result.statusCode).toBe(200);
    expect(result.payload?.body).toContain('Processed: 84');
  });

  it('should capture snapshot with line_capture data', () => {
    const snapshotLog = result.logs?.find((log: any) =>
      log.attributes.message.includes('Snapshot') &&
      log.attributes.message.includes('line_capture') &&
      log.attributes.message.includes(PROBE_ID)
    );
    expect(snapshotLog).toBeDefined();
    console.log('✅ Snapshot log found');
  });

  it('should capture local variable value=42 in snapshot', () => {
    const snapshotLog = result.logs?.find((log: any) =>
      log.attributes.message.includes('Snapshot') &&
      log.attributes.message.includes('line_capture')
    );
    const snapshotMessage = snapshotLog?.attributes?.message || '';
    expect(snapshotMessage).toContain("'value'");
    expect(snapshotMessage).toMatch(/'value'.*42/);
    console.log('✅ Snapshot captured local variable value=42');
  });

  it('should capture local variable multiplied=84 in snapshot', () => {
    const snapshotLog = result.logs?.find((log: any) =>
      log.attributes.message.includes('Snapshot') &&
      log.attributes.message.includes('line_capture')
    );
    const snapshotMessage = snapshotLog?.attributes?.message || '';
    expect(snapshotMessage).toContain("'multiplied'");
    expect(snapshotMessage).toMatch(/'multiplied'.*84/);
    console.log('✅ Snapshot captured local variable multiplied=84');
  });


});
