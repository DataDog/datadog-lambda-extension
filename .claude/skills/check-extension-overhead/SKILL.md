---
name: check-extension-overhead
description: Read the overhead of released extension versions from the release dashboard, Lambda Extension Overhead per Runtime, and the size of each released layer. Use it before a release, or to find in which release the init duration, the duration, the post-runtime duration or the layer size changed.
---

# Check the overhead of released extension versions

The team checks the dashboard
[Lambda Extension Overhead per Runtime](https://ddserverless.datadoghq.com/dashboard/utb-gvr-bqv/lambda-extension-overhead-per-runtime)
before each release. It is in the Datadog Serverless org, `ddserverless.datadoghq.com`. Its data
comes from the self-monitoring functions of the `serverless-self-monitoring` repository, project
`lambda-on-demand`. Those functions run in pairs: one with the extension and one without.

## How to read the dashboard

Get the data with a browser that is logged in to the org, for example with the Playwright tools.
Open any page of `ddserverless.datadoghq.com`. If the page goes to the login page, ask the user
to log in in that browser window. Do not use `dd-auth` for the duration metrics. On 2026-10-19,
`dd-auth` with this org returned no series for any `aws.lambda.*` metric, while the dashboard
showed data. The layer size metric worked through `dd-auth`.

In the logged-in page, call the metrics query API with the session cookie (`browser_evaluate`):

```js
async () => {
  const q = 'avg:aws.lambda.enhanced.post_runtime_duration{service:self-monitoring-lod,env:prod,fips:false,stack:agent,traffic:1min} by {extension_version}.rollup(avg,10800)';
  const from = Math.floor(Date.parse('2026-09-12T00:00:00Z') / 1000), to = Math.floor(Date.parse('2026-09-27T00:00:00Z') / 1000);
  const r = await fetch(`/api/v1/query?from=${from}&to=${to}&query=${encodeURIComponent(q)}`, { credentials: 'include' });
  const d = await r.json();
  return d.series.map(s => s.scope + ': ' + s.pointlist.filter(p => p[1] != null)
    .map(p => new Date(p[0]).toISOString().slice(5, 13) + '=' + p[1].toFixed(0)).join(' ')).join('\n');
}
```

Replace the dashboard's template variables with tag filters, for example `env:prod` for `$env`.
`.rollup(avg,<seconds>)` sets the step. Use 86400 for days and 10800 or 3600 to find when a
change started. Group by `runtime` or `architecture` to see whether a change is in all functions.

To open one widget for some versions, put these in the URL:

| URL parameter | Example |
| --- | --- |
| `tpl_var_extension[N]`, one for each version | `tpl_var_extension%5B0%5D=100&tpl_var_extension%5B1%5D=99` |
| `fullscreen_widget` and `fullscreen_section=overview` | `fullscreen_widget=2635210694743807` (Post Runtime Extension Duration) |
| `from_ts`, `to_ts` (epoch milliseconds) and `live=false` | A time range that includes all the versions |

To look at the widget itself, a full-screen widget has a legend table with the average, minimum and maximum of each series.
Read it from the page snapshot (`browser_snapshot` with a `filename`, then search the file).
The table is more exact than a screenshot. Each row starts with the `extension_version`.

## The three metrics to check

All queries filter on `service:self-monitoring-lod` and the template variables `env` (default
`prod`), `runtime`, `extension` (tag `extension_version`) and `fips` (default `false`). The
first group of widgets uses `stack:agent`, which has the extension and no tracer.

| Widget | Query | Unit |
| --- | --- | --- |
| Init Duration Overhead | `aws.lambda.enhanced.init_duration`, `stack:agent` minus `stack:baseline`, both `traffic:cold` | second |
| Duration Overhead | `aws.lambda.enhanced.duration{stack:agent,traffic:cold}`. The number widget shows the duration itself, not a difference. The time series shows `stack:baseline` next to it. | second |
| Post Runtime Extension Duration | `aws.lambda.enhanced.post_runtime_duration{stack:agent}`, `traffic:cold` in the number widget, `traffic:busy` and `traffic:1min` in the time series | millisecond |

The two traffic patterns of post-runtime duration show different work:

- `traffic:1min` has one invocation each minute. The extension flushes during every
  invocation, so this line shows the time of one flush, about 80 ms.
- `traffic:busy` has one invocation each second. After 20 invocations, the default flush
  strategy flushes in the background, so this line is about 4 ms.

The time series widgets group by `extension_version`, so one view shows each version as a line.
The number widgets show the average over the selected time range. To get the number for one
version, set the `extension` template variable to it.

`extension_version` is the version number of the layer, which is the `N` of the release tag
`vN`. Layer versions 100 and 101 were each created 1 to 2 days after the tag with the same
number.

The `stack:baseline` functions have no extension. The Datadog Forwarder sends their logs.

The second group of widgets, under "with tracing", uses `stack:tracing`, which adds the tracer
layer. A change there and not in the first group can come from the tracer, the Lambda library, or
the extension's handling of traces.

## Check the time before you blame a version

The dashboard puts each version on its own line, so a change can look like it comes with a
release. It can also come from outside the extension, for example the Datadog intake or AWS.
Before you report a regression for a version, find the time when the change started, in steps of
3 hours. Then check whether functions whose extension did not change have the same step at the
same time, for example `env:dev` functions that stayed on one dev version. Step 2 of
`bisect-extension-regression` has the details.

The prod `stack:agent` functions of `traffic:1min` were all `x86_64`, on 7 runtimes, in `us-east-1`.

## Layer size

The dashboard has no layer size. Get the size of released versions from AWS:

```sh
aws-vault exec sso-serverless-sandbox-account-admin -- python3 .claude/skills/measure-extension-layer/scripts/layer_size.py v99 v100 v101
```

It prints the zipped size (`CodeSize`, the number that AWS limits) and the unzipped size. Use
`--arch amd64` or `--fips` for the other layers.

The metric `sls.monitoring.latest_layer_version_size{layer:datadog-extension-arm}`, grouped by
`layer_version`, has the same zipped size. It has a value only for versions that were the latest
release while the layer version updater ran, so `layer_size.py` is more complete.

## Report

For each metric, give the value of each version that you compared and the time range. A change
between two versions is a finding only if it is larger than the spread of the time series for
one version. To find the cause, use the `bisect-extension-regression` skill.
