# Log Levels by Target

`DD_LOG_LEVEL_BY_TARGET` turns on more logs for one part of the extension, without the debug logs of all other parts. It is mainly for Datadog engineers and support, to collect logs for a support ticket. The Datadog documentation site does not describe it.

Set these environment variables on the Lambda function:

| Environment variable | Value |
| --- | --- |
| `DD_LOG_LEVEL` | The level for all other logs, for example `info`. The default is `info`. |
| `DD_LOG_LEVEL_BY_TARGET` | A comma-separated list of `<target>=<level>` entries |

A level is `off`, `error`, `warn`, `info`, `debug` or `trace`. For example, if `DD_LOG_LEVEL` is `info` and `DD_LOG_LEVEL_BY_TARGET` is `cold_start_duration=debug`, the extension logs at `info` level as usual, and also logs the duration of each cold start step.

An entry matches each target that starts with its text. For an entry that is not valid, the extension logs a warning that names the entry, and skips it.

Use the targets that `mod.rs` defines, such as `cold_start_duration`. Other targets are module paths and crate names, which change when the code moves.
