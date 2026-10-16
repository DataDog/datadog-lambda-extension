# Log Levels by Target

## What it is for

`DD_LOG_LEVEL_BY_TARGET` turns on more logs for one part of the extension, without the debug logs of all other parts. It is mainly for Datadog engineers and support, to collect logs for a support ticket. A customer can also set it. The Datadog documentation site does not describe it.

## How to set it

Set these environment variables on the Lambda function:

| Environment variable | Value |
| --- | --- |
| `DD_LOG_LEVEL` | The level for all other logs, for example `info`. The default is `info`. |
| `DD_LOG_LEVEL_BY_TARGET` | A comma-separated list of `<target>=<level>` entries |

A level is `off`, `error`, `warn`, `info`, `debug` or `trace`. For example, the value `cold_start_duration=debug` adds the duration of each cold start step to the `info` logs.

An entry matches each target that starts with its text. For example, `h2=debug` matches all the `h2::…` targets.

The extension skips an entry that is not valid, and logs a warning such as `Ignoring invalid DD_LOG_LEVEL_BY_TARGET entries: ["cold_start_duration=verbose"]`. An entry with no target, such as `debug`, is not valid.

## Targets

| Kind of target | Example entry | Logs that you get | Name can change? |
| --- | --- | --- | --- |
| Custom target that a log statement sets | `cold_start_duration=debug` | The duration of each cold start step | No. We keep these names. |
| HTTP library, a third-party crate | `hyper_util=debug,h2=debug` | Each TCP connect, and each HTTP/2 frame | Yes, when we update the crate |
| Module path in the extension | `bottlecap::traces=debug` | The debug logs of that module | Yes, when we move code |
| Crate from serverless-components | `dogstatsd=debug` | The debug logs of that crate | Yes, when we move code |

Give a customer only the custom targets, and the HTTP library entries in the next section. The other names are internal, so their meaning can change in any release.

The extension sends its own logs to Datadog, but it does not send the lines of the HTTP libraries `h2`, `hyper`, `reqwest` and `rustls`. Their lines go to CloudWatch only. These libraries log each request, including the requests that send logs, so sending their lines would add lines to each flush.

## Find where an upload to Datadog stops

Use this when invocations run until their timeout after the handler returns, and the extension's last line is `Attempting request`.

| Environment variable | Value |
| --- | --- |
| `DD_LOG_LEVEL` | `debug` |
| `DD_LOG_LEVEL_BY_TARGET` | `hyper_util=debug,h2=debug` |

Then read the last HTTP library lines before the stall in CloudWatch. These lines start with `{"target":"hyper_util…` or `{"target":"h2…`.

| Last lines | Where the upload stopped |
| --- | --- |
| `connecting to <ip>:<port>`, and no `connected to <ip>:<port>` | TCP connect |
| `connected to <ip>:<port>`, and no `binding client connection` | TLS handshake |
| `send frame=Data { … END_STREAM }`, and no `received frame=Headers` | The server did not reply |

The `h2` lines come only from the HTTP/2 uploads. By default, the metrics and logs uploads use HTTP/2. With a proxy or `DD_HTTP_PROTOCOL=http1`, they use HTTP/1. The trace upload always uses HTTP/1, so it shows only the `hyper_util` connect lines. The `rustls` target writes no lines, because release builds of rustls do not include its logging code.

`h2=debug` writes a line for each HTTP/2 frame, which increases the CloudWatch logs of the function. Remove both variables after you collect the logs.

## For developers

The crate that parses the entries and builds the log filter is `datadog-serverless-logging` in [serverless-components](https://github.com/DataDog/serverless-components). Its README lists the parsing rules.

To add a custom target, add a constant next to `COLD_START_DURATION` in `mod.rs`. Because of the prefix match, the name of a new target must not start with the name of a target that exists. For example, an entry for `trace_flush` would also match `trace_flush_duration`.
