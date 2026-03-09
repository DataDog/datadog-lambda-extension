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

    it('should invoke Lambda successfully', () => {
      expect(result.statusCode).toBe(200);
    });

    it('should have "Hello from non-durable function!" log message', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from non-durable function!')
      );
      expect(log).toBeDefined();
    });

    it('logs should have lambda.request_id attribute', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from non-durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.request_id).toBeDefined();
    });

    it('logs should NOT have lambda.durable_execution_id attribute', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from non-durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeUndefined();
    });

    it('logs should NOT have lambda.durable_execution_name attribute', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from non-durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeUndefined();
    });

    it('platform START log should NOT have lambda.durable_execution_id or lambda.durable_execution_name attributes', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('START RequestId:')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeUndefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeUndefined();
    });

    it('platform END log should NOT have lambda.durable_execution_id or lambda.durable_execution_name attributes', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('END RequestId:')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeUndefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeUndefined();
    });

    it('extension logs should NOT have lambda.durable_execution_id or lambda.durable_execution_name attributes', () => {
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
    let coldStartResult: LambdaInvocationDatadogData;
    let warmStartResult: LambdaInvocationDatadogData;
    let concurrentResults: LambdaInvocationDatadogData[];

    beforeAll(async () => {
      const identifier = getIdentifier();
      const baseFunctionName = `integ-${identifier}-durable-python-durable-lambda`;

      console.log(`CloudWatch log group: /aws/lambda/${baseFunctionName}`);
      // Durable functions require a qualified ARN (version or alias); publish a new version
      // so the first invocation is a cold start and so we have a valid qualifier.
      console.log('Publishing version for durable Python Lambda...');
      const version = await publishVersion(baseFunctionName);
      const functionName = `${baseFunctionName}:${version}`;

      console.log('Invoking durable Python Lambda (cold start - first invocation of new version)...');
      coldStartResult = await invokeLambdaAndGetDatadogData(functionName, {}, false);

      console.log('Invoking durable Python Lambda (warm start)...');
      warmStartResult = await invokeLambdaAndGetDatadogData(functionName, {}, false);

      console.log('Invoking durable Python Lambda 3x concurrently...');
      concurrentResults = await Promise.all([
        invokeLambdaAndGetDatadogData(functionName, {}, false),
        invokeLambdaAndGetDatadogData(functionName, {}, false),
        invokeLambdaAndGetDatadogData(functionName, {}, false),
      ]);

      console.log('All durable Lambda invocations and data fetching completed');
    }, 600000); // 10 minute timeout

    it('logs should have lambda.durable_execution_id attribute', () => {
      const log = coldStartResult.logs?.find((log: any) =>
        log.message.includes('Hello from durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
    });

    it('logs should have lambda.durable_execution_name attribute', () => {
      const log = coldStartResult.logs?.find((log: any) =>
        log.message.includes('Hello from durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
    });

    it('logs arriving before the aws.lambda span should be held and released with durable execution context', () => {
      // On cold start, function logs likely arrive before the tracer flushes the aws.lambda span.
      // The extension stashes these logs in held_logs[request_id] (because is_durable_function
      // is Some(true) and the request_id is not yet in durable_context_map). Once the span
      // arrives and context is inserted, drain_held_for_request_id decorates and releases them.
      const log = coldStartResult.logs?.find((log: any) =>
        log.message.includes('Hello from durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
    });

    it('logs arriving after the aws.lambda span should be decorated immediately with durable execution context', () => {
      // In a durable orchestration a function may be invoked multiple times within the same
      // execution. On the second (warm-start) invocation the extension already has the
      // durable_execution_id / durable_execution_name in its durable_context_map from the
      // first invocation's span. Any log whose request_id is already in the map is therefore
      // decorated inline in queue_log_after_rules without ever being held.
      const log = warmStartResult.logs?.find((log: any) =>
        log.message.includes('Hello from durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
    });

    it('each invocation should have its own durable execution context when multiple concurrent invocations occur', () => {
      // Concurrent invocations run in separate Lambda execution environments, each with its
      // own extension state and durable_context_map. Every invocation independently receives
      // the is_durable_function flag and the aws.lambda span, so each set of logs is decorated
      // with its own (potentially distinct) durable execution context.
      for (const result of concurrentResults) {
        const log = result.logs?.find((log: any) =>
          log.message.includes('Hello from durable function!')
        );
        expect(log).toBeDefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
        expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
      }

      // Each concurrent invocation should report a valid request_id so we can distinguish
      // their log sets from one another.
      const requestIds = concurrentResults.map(r => r.requestId);
      const uniqueRequestIds = new Set(requestIds);
      expect(uniqueRequestIds.size).toBe(concurrentResults.length);
    });

    it('platform START log should have lambda.durable_execution_id and lambda.durable_execution_name attributes', () => {
      // PlatformStart generates "START RequestId: ... Version: ..." with a request_id.
      // In durable mode the log is held in held_logs until the aws.lambda span arrives,
      // then decorated with durable execution context and released.
      const log = coldStartResult.logs?.find((log: any) =>
        log.message.includes('START RequestId:')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
    });

    it('platform END log should have lambda.durable_execution_id and lambda.durable_execution_name attributes', () => {
      // PlatformRuntimeDone generates "END RequestId: ..." with a request_id and is handled
      // identically to START — held until durable context arrives, then decorated.
      const log = coldStartResult.logs?.find((log: any) =>
        log.message.includes('END RequestId:')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_id).toBeDefined();
      expect(log?.attributes?.attributes?.lambda?.durable_execution_name).toBeDefined();
    });

    it('extension logs should have lambda.durable_execution_id and lambda.durable_execution_name attributes', () => {
      // Extension logs (debug!/info!/warn! from the Rust extension code) arrive without a
      // request_id and are stashed as orphan logs. When PlatformStart fires they inherit the
      // invocation request_id, then follow the same held-log path as other durable logs:
      // held until the aws.lambda span provides durable execution context, then decorated.
      // DD_LOG_LEVEL=DEBUG ensures these logs are emitted and appear in Datadog.
      const logs = coldStartResult.logs?.filter((log: any) =>
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
