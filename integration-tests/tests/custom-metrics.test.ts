import { hasMetricWithTag } from "./utils/datadog";
import { forceColdStart, invokeLambda } from "./utils/lambda";
import { getIdentifier, DEFAULT_DATADOG_INDEXING_WAIT_MS } from "../config";

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
    const functionNames = [unfilteredFunctionName, filteredFunctionName];

    await Promise.all(functionNames.map((fn) => forceColdStart(fn)));

    // Back up the query window by 60s so the metric bucket (which Datadog
    // aligns to the rollup interval boundary, often before the invocation)
    // falls inside the range we pass to /api/v1/query.
    invocationStartTime = Date.now() - 60_000;

    await Promise.all(functionNames.map((fn) => invokeLambda(fn)));

    await new Promise((resolve) =>
      setTimeout(resolve, DEFAULT_DATADOG_INDEXING_WAIT_MS),
    );

    metricsEndTime = Date.now();

    console.log("Lambdas invoked and indexing wait complete");
  }, 900000);

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
