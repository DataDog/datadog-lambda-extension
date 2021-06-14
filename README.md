# datadog-lambda-extension
[![Slack](https://chat.datadoghq.com/badge.svg?bg=632CA6)](https://chat.datadoghq.com/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/DataDog/datadog-agent/blob/master/LICENSE)

AWS Lambda Extension that supports submitting custom metrics, traces and logs synchronously while your AWS Lambda function executes. 

## Installation

Follow the [installation instructions](https://docs.datadoghq.com/serverless/datadog_lambda_library/extension/), and view your function's enhanced metrics, traces and logs in Datadog.

## Environment Variables

### DD_API_KEY

The Datadog API Key must be defined by setting one of the following environment variables:

- DD_API_KEY - the Datadog API Key in plain-text, NOT recommended
- DD_KMS_API_KEY - the KMS-encrypted API Key, requires the `kms:Decrypt` permission

### DD_SITE

Possible values are `datadoghq.com`, `datadoghq.eu`, `us3.datadoghq.com` and `ddog-gov.com`. The default is `datadoghq.com`.

### DD_LOG_LEVEL

Set to `debug` enable debug logs from the Datadog Lambda Extension. Defaults to `info`.

## Configuration file

Using a configuration file is supported (note that environment variable settings
override the value from the configuration file): you only have to create a [`datadog.yaml`](https://docs.datadoghq.com/agent/guide/agent-configuration-files/?tab=agentv6v7)
file in the root of your lambda function.

### Logs filtering and scrubbing support

The Datadog Lambda Extension supports using the different logs filtering / scrubbing features of the Datadog Agent. All you have to do is to set `processing_rules` in the `logs_config`
field of your configuration.

Please refer to the public documentation for all filtering and scrubbing features:

* https://docs.datadoghq.com/agent/logs/advanced_log_collection/?tab=configurationfile#global-processing-rules
* https://docs.datadoghq.com/agent/logs/advanced_log_collection/?tab=configurationfile#filter-logs

## Opening Issues

If you encounter a bug with this package, we want to hear about it. Before opening a new issue, search the existing issues to avoid duplicates.

When opening an issue, include the Datadog Lambda Layer version, and stack trace if available. In addition, include the steps to reproduce when appropriate.

You can also open an issue for a feature request.


## Contributing

If you find an issue with this package and have a fix, please feel free to open a pull request following the [procedures](https://github.com/DataDog/datadog-agent/blob/master/docs/dev/contributing.md).

## Community

For product feedback and questions, join the `#serverless` channel in the [Datadog community on Slack](https://chat.datadoghq.com/).

## License

Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.

This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021 Datadog, Inc.
