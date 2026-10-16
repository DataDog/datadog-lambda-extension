# Log Levels by Target

`DD_LOG_LEVEL_BY_TARGET` turns on more logs for one part of the extension, without the debug logs of all other parts. It is mainly for Datadog engineers and support, to collect logs for a support ticket. The Datadog documentation site does not describe it.

Set these environment variables on the Lambda function:

| Environment variable | Value |
| --- | --- |
| `DD_LOG_LEVEL` | The level for all other logs, for example `info`. The default is `info`. |
| `DD_LOG_LEVEL_BY_TARGET` | A comma-separated list of `<target>=<level>` entries |

A level is `off`, `error`, `warn`, `info`, `debug` or `trace`. For example, `cold_start_duration=debug` adds the duration of each cold start step to the `info` logs.

An entry matches each target that starts with its text. The extension skips an entry that is not valid, and logs a warning that names the entry.

Only the custom targets, such as `cold_start_duration`, keep their names. The names of modules and crates can change in any release.
