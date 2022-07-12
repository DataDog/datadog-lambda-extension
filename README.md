# datadog-lambda-extension
[![Slack](https://chat.datadoghq.com/badge.svg?bg=632CA6)](https://chat.datadoghq.com/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/DataDog/datadog-agent/blob/master/LICENSE)

**Note:** This repository contains release notes, issues, instructions, and scripts related to the Datadog Lambda Extension. The extension is a special build of the Datadog Agent. The source code can be found [here](https://github.com/DataDog/datadog-agent/tree/main/cmd/serverless). 

The Datadog Lambda Extension is an AWS Lambda Extension that supports submitting custom metrics, traces, and logs asynchronously while your AWS Lambda function executes.

## Installation

Follow the [installation instructions](https://docs.datadoghq.com/serverless/installation), and view your function's enhanced metrics, traces and logs in Datadog.

## Upgrading
Upgrading is as easy as bumping up the Datadog Extension version in your Lambda layer configurations or Dockerfile (for Lambda functions deployed as container images). View the latest [releases](https://github.com/DataDog/datadog-lambda-extension/releases) for changelog before upgrading.

## Configurations

Follow the [configuration instructions](https://docs.datadoghq.com/serverless/configuration) to tag your telemetry, capture request/response payloads, filter or scrub sensitive information from logs or traces, and more.

## Overhead

The Datadog Lambda Extension introduces a small amount of overhead to your Lambda function's cold starts (that is, the higher init duration), as the Extension needs to initialize. Datadog is continuously optimizing the Lambda extension performance and recommend always using the latest release.

You may notice an increase of your Lambda function's reported duration. This is because the Datadog Lambda Extension needs to flush data back to the Datadog API. Although the time spent by the extension flushing data is reported as part of the duration, it's done *after* AWS returns your function's response back to the client. In other words, the added duration *does not slow down* your Lambda function. See this [AWS blog post](https://aws.amazon.com/blogs/compute/performance-and-functionality-improvements-for-aws-lambda-extensions/) for more technical information. To monitor your function's actual performance and exclude the duration added by the Datadog extension, use the metric `aws.lambda.enhanced.runtime_duration`.

By default, the Extension flushes data back to Datadog at the end of each invocation (for example, cold starts always trigger flushing). This avoids delays of data arrival for sporadic invocations from low-traffic applications, cron jobs, and manual tests. Once the Extension detects a steady and frequent invocation pattern (more than once per minute), it batches data from multiple invocations and flushes periodically at the beginning of the invocation when it's due. This means that *the busier your function is, the lower the average duration overhead per invocation*. In other words, for low-traffic applications, the duration overhead would be noticeable while the associated cost overhead is typically negligible; for high-traffic applications, the duration overhead would be barely noticeable.   

For Lambda functions deployed in a region that is far from the Datadog site, for example, a Lambda function deployed in eu-west-1 reporting data to the US1 Datadog site, can observe a higher duration overhead due to the network latency.

## Opening Issues

If you encounter a bug with this package, we want to hear about it. Before opening a new issue, search the existing issues to avoid duplicates.

When opening an issue, include the Extension version, and stack trace if available. In addition, include the steps to reproduce when appropriate.

You can also open an issue for a feature request.

## Contributing

If you find an issue with this package and have a fix, please feel free to open a pull request following the [procedures](https://github.com/DataDog/datadog-agent/blob/master/docs/dev/contributing.md).

## Community

For product feedback and questions, join the `#serverless` channel in the [Datadog community on Slack](https://chat.datadoghq.com/).

## License

Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.

This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021 Datadog, Inc.
