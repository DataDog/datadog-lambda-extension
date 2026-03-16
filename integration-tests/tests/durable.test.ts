import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { publishVersion } from './utils/lambda';
import { getIdentifier } from '../config';

describe('Durable Function Log Tests', () => {
  describe('Non-Durable Function', () => {
    let result: LambdaInvocationDatadogData;

    beforeAll(async () => {
      const identifier = getIdentifier();
      const functionName = `integ-${identifier}-durable-python-lambda`;

      console.log(`CloudWatch log group: /aws/lambda/${functionName}`);
      console.log('Invoking non-durable Python Lambda...');
      result = await invokeLambdaAndGetDatadogData(functionName, {}, true);
      console.log('Non-durable Lambda invocation and data fetching completed');
    }, 300000); // 5 minute timeout

    it('should invoke Lambda successfully and log message should have request_id but no durable execution context', () => {
      expect(result.statusCode).toBe(200);
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from non-durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.request_id).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeUndefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeUndefined();
    });

    it('platform logs should NOT have durable execution context', () => {
      for (const marker of ['START RequestId:', 'END RequestId:']) {
        const log = result.logs?.find((log: any) => log.message.includes(marker));
        expect(log).toBeDefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeUndefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeUndefined();
      }
    });

    it('extension logs should NOT have durable execution context', () => {
      // Extension logs (from debug!/info!/warn! in the Rust extension code) are forwarded
      // in non-durable mode. They arrive without a request_id initially (orphan logs), then
      // inherit the invocation request_id when PlatformStart fires.
      // DD_LOG_LEVEL=DEBUG ensures these logs appear in Datadog.
      const logs = result.logs?.filter((log: any) =>
        log.message.includes('DD_EXTENSION')
      );
      expect(logs).toBeDefined();
      expect(logs?.length).toBeGreaterThan(0);
      for (const log of logs ?? []) {
        expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeUndefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeUndefined();
      }
    });
  });

  // These tests require a Lambda runtime environment that reports 'DurableFunction' in
  // PlatformInitStart.runtime_version so that the extension sets is_durable_function = true
  // and activates the log-holding / durable-context-decorating pipeline.
  describe('Durable Function', () => {
    let result: LambdaInvocationDatadogData;

    beforeAll(async () => {
      const identifier = getIdentifier();
      const baseFunctionName = `integ-${identifier}-durable-python-durable-lambda`;

      // Durable functions require a qualified ARN (version or alias); publish a new version
      // to obtain a valid qualifier.
      console.log('Publishing version for durable Python Lambda...');
      const version = await publishVersion(baseFunctionName);
      const functionName = `${baseFunctionName}:${version}`;

      console.log('Invoking durable Python Lambda...');
      result = await invokeLambdaAndGetDatadogData(functionName, {}, false);
      console.log('Durable Lambda invocation and data fetching completed');
    }, 300000); // 5 minute timeout

    it('function log should have durable execution context', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
    });

    it('platform logs should have durable execution context', () => {
      for (const marker of ['START RequestId:', 'END RequestId:']) {
        const log = result.logs?.find((log: any) => log.message.includes(marker));
        expect(log).toBeDefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
      }
    });

    it('extension logs should have durable execution context', () => {
      // Extension logs (debug!/info!/warn! from the Rust extension code) arrive without a
      // request_id and are stashed as orphan logs. When PlatformStart fires they inherit the
      // invocation request_id, then follow the same held-log path as other durable logs:
      // held until the aws.lambda span provides durable execution context, then decorated.
      // DD_LOG_LEVEL=DEBUG ensures these logs are emitted and appear in Datadog.
      const logs = result.logs?.filter((log: any) =>
        log.message.includes('DD_EXTENSION')
      );
      expect(logs).toBeDefined();
      expect(logs?.length).toBeGreaterThan(0);
      for (const log of logs ?? []) {
        expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
      }
    });
  });
});
