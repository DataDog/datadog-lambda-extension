import { invokeAndCollectTelemetry, FunctionConfig } from "./utils/default";
import { hasMetricWithTag } from "./utils/datadog";
import { forceColdStart } from "./utils/lambda";
import { getIdentifier } from "../config";

const identifier = getIdentifier();
const stackName = `integ-${identifier}-custom-metrics`;

const CUSTOM_METRIC_NAME = "custom.exclude_tags_test";
const EXCLUDED_TAGS = ["function_arn", "region"];
const KEPT_TAGS = ["functionname", "account_id"];

describe("Customer Metrics Exclude Tags Integration Tests", () => {
  let invocationStartTime: number;
  let metricsEndTime: number;

  const unfilteredFunctionName = `${stackName}-unfiltered-lambda`;
  const filteredFunctionName = `${stackName}-filtered-lambda`;

  beforeAll(async () => {
    const functions: FunctionConfig[] = [
      { functionName: unfilteredFunctionName, runtime: "unfiltered" },
      { functionName: filteredFunctionName, runtime: "filtered" },
    ];

    await Promise.all(functions.map((fn) => forceColdStart(fn.functionName)));

    invocationStartTime = Date.now();

    await invokeAndCollectTelemetry(functions, 1, 1, 0);

    metricsEndTime = Date.now();

    console.log("All invocations and data fetching completed");
  }, 600000);

  describe("unfiltered function (no DD_CUSTOMER_METRICS_EXCLUDE_TAGS)", () => {
    it.each(EXCLUDED_TAGS)(
      "should have %s tag on custom metric",
      async (tag) => {
        const hasTag = await hasMetricWithTag(
          CUSTOM_METRIC_NAME,
          unfilteredFunctionName,
          `${tag}:*`,
          invocationStartTime,
          metricsEndTime,
        );
        expect(hasTag).toBe(true);
      },
    );

    it.each(KEPT_TAGS)("should have %s tag on custom metric", async (tag) => {
      const hasTag = await hasMetricWithTag(
        CUSTOM_METRIC_NAME,
        unfilteredFunctionName,
        `${tag}:*`,
        invocationStartTime,
        metricsEndTime,
      );
      expect(hasTag).toBe(true);
    });
  });

  describe("filtered function (DD_CUSTOMER_METRICS_EXCLUDE_TAGS=function_arn,region)", () => {
    it.each(EXCLUDED_TAGS)(
      "should NOT have %s tag on custom metric",
      async (tag) => {
        const hasTag = await hasMetricWithTag(
          CUSTOM_METRIC_NAME,
          filteredFunctionName,
          `${tag}:*`,
          invocationStartTime,
          metricsEndTime,
        );
        expect(hasTag).toBe(false);
      },
    );

    it.each(KEPT_TAGS)(
      "should still have %s tag on custom metric",
      async (tag) => {
        const hasTag = await hasMetricWithTag(
          CUSTOM_METRIC_NAME,
          filteredFunctionName,
          `${tag}:*`,
          invocationStartTime,
          metricsEndTime,
        );
        expect(hasTag).toBe(true);
      },
    );
  });
});
