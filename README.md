# datadog-lambda-extension
[![Slack](https://chat.datadoghq.com/badge.svg?bg=632CA6)](https://chat.datadoghq.com/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/DataDog/datadog-agent/blob/master/LICENSE)

> :information_source: **Note:** This repository contains release notes, issues, instructions, and scripts related to the Datadog Lambda Extension. The extension is a special build of the Datadog Agent. The source code can be found [here](https://github.com/DataDog/datadog-agent/tree/main/cmd/serverless). 

The Datadog Lambda Extension is an AWS Lambda Extension that supports submitting custom metrics, traces, and logs asynchronously while your AWS Lambda function executes.

## Installation

Follow the [installation instructions](https://docs.datadoghq.com/serverless/installation), and view your function's enhanced metrics, traces and logs in Datadog.

## Configurations

Follow the [configuration instructions][https://docs.datadoghq.com/serverless/configuration] to tag your telemetry, capture request/response payloads, filter or scrub sensitive information from logs or traces, and more.

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
