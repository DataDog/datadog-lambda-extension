import { invokeLambdaAndGetDatadogData, LambdaInvocationDatadogData } from './utils/util';
import { getIdentifier } from '../config';

describe('Durable Function Log Tests', () => {
  describe('Non-Durable Function', () => {
    let result: LambdaInvocationDatadogData;

    beforeAll(async () => {
      const identifier = getIdentifier();
      const functionName = `integ-${identifier}-durable-python-lambda`;

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
      expect(log?.attributes?.lambda?.request_id).toBeDefined();
    });

    it('logs should NOT have lambda.durable_execution_id attribute', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from non-durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.lambda?.durable_execution_id).toBeUndefined();
    });

    it('logs should NOT have lambda.durable_execution_name attribute', () => {
      const log = result.logs?.find((log: any) =>
        log.message.includes('Hello from non-durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.lambda?.durable_execution_name).toBeUndefined();
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
      const functionName = `integ-${identifier}-durable-python-durable-lambda`;

      console.log('Invoking durable Python Lambda (cold start)...');
      coldStartResult = await invokeLambdaAndGetDatadogData(functionName, {}, true);

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
      expect(log?.attributes?.lambda?.durable_execution_id).toBeDefined();
    });

    it('logs should have lambda.durable_execution_name attribute', () => {
      const log = coldStartResult.logs?.find((log: any) =>
        log.message.includes('Hello from durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.lambda?.durable_execution_name).toBeDefined();
    });

    it('logs arriving before the aws.lambda span should be held and released with durable execution context', () => {
      // On cold start, function logs arrive before the tracer flushes the aws.lambda span.
      // The extension stashes these logs in held_logs[request_id] (because is_durable_function
      // is Some(true) and the request_id is not yet in durable_context_map). Once the span
      // arrives and context is inserted, drain_held_for_request_id decorates and releases them.
      const log = coldStartResult.logs?.find((log: any) =>
        log.message.includes('Hello from durable function!')
      );
      expect(log).toBeDefined();
      expect(log?.attributes?.lambda?.durable_execution_id).toBeDefined();
      expect(log?.attributes?.lambda?.durable_execution_name).toBeDefined();
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
      expect(log?.attributes?.lambda?.durable_execution_id).toBeDefined();
      expect(log?.attributes?.lambda?.durable_execution_name).toBeDefined();
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
        expect(log?.attributes?.lambda?.durable_execution_id).toBeDefined();
        expect(log?.attributes?.lambda?.durable_execution_name).toBeDefined();
      }

      // Each concurrent invocation should report a valid request_id so we can distinguish
      // their log sets from one another.
      const requestIds = concurrentResults.map(r => r.requestId);
      const uniqueRequestIds = new Set(requestIds);
      expect(uniqueRequestIds.size).toBe(concurrentResults.length);
    });
  });
});
