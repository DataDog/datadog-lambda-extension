---
name: measure-extension-layer
description: Measure the size, cold start and duration overhead of extension layers in the serverless sandbox AWS account. A layer can be a release (v101) or a build of a branch. Use it to test a release candidate or a change for a regression, to compare two versions, or as the test step of a bisect.
---

# Measure the size and cold start of an extension layer

The scripts in `scripts/` create small Lambda functions, one or more for each variant, in the
serverless sandbox account. The default region is `us-east-2`. They force cold starts and read the `REPORT` lines from
CloudWatch. A variant is a name and a layer. Always measure the variants that you compare in
the same run. Each round does one cold start of each variant, so a change over time affects all
variants the same way.

Run the scripts through `aws-vault exec sso-serverless-sandbox-account-admin --`. They need
`boto3`.

`measure.py` needs a Datadog API key in the environment variable `DD_API_KEY`. Without a key,
the extension does not flush, so the run measures no flush time. The test functions send their
data to the org of that key. The script stores the key in a Secrets Manager secret,
`<user>-extension-perf-api-key`, in the region of the run. The functions read the key from that
secret, as the self-monitoring functions do.

## 1. Get the layers

Each variant is `NAME=SPEC`. SPEC is one of these:

| SPEC | Layer |
| --- | --- |
| `none` | No extension. Always include `noext=none`. The overhead of the extension is the difference from this variant. |
| `v101` | The released layer of that version. |
| a layer version ARN | A layer that you built. It must be in the region of the run, and it must match `--arch`. |

To build and publish a layer from a branch, use step 3 of the `build-custom-layer` skill with
`REGION=us-east-2` and `SUFFIX=<your-name>-perf`. You do not need to make it public. Docker must
be running. For a size check only, build without publishing:

```sh
ARCHITECTURE=arm64 FIPS=0 ./scripts/build_bottlecap_layer.sh
python3 .claude/skills/measure-extension-layer/scripts/layer_size.py --zip .layers/datadog_extension-arm64.zip
```

`build_bottlecap_layer.sh` needs `FIPS` set. Check that the zip is new before you use it. A
build with a warm cargo cache took about 1 minute.

Compare the zipped size only between layers that you built the same way. For v101, a local
build had the same unzipped size as the released layer within 5 KB, but its zip was 252 KB
smaller. The cause of that difference is not known. So to measure what a change adds, build the
base commit too.

## 2. Run the measurement

```sh
aws-vault exec sso-serverless-sandbox-account-admin -- python3 .claude/skills/measure-extension-layer/scripts/measure.py \
  --out /tmp/extension-perf/<label> -n 20 --warm 3 \
  noext=none old=v101 new=arn:aws:lambda:us-east-2:425362996713:layer:Datadog-Bottlecap-Beta-ARM-<your-name>-perf:1
```

| Option | Meaning |
| --- | --- |
| `-n` | Cold starts for each variant. The default is 20. |
| `--warm` | Warm invocations after each cold start. Use it to measure the duration of a warm invocation. |
| `--warm-interval` | Seconds between warm invocations. The default is 1. All variants share each pause, so a long interval does not cost more for more variants. |
| `--runtime` | `python` (Python 3.13, the default), `python314` (Python 3.14, as in the self-monitoring functions) or `node` (Node.js 24). The handler returns at once, as in the self-monitoring functions. |
| `--arch` | `arm64` (the default) or `x86_64`. |
| `--region` | `us-east-2` (the default) or another region. Every layer ARN must be in this region. The self-monitoring functions are in `us-east-1`. |
| `--copies` | Functions for each variant. The default is 1. More copies give more samples in the same time, because all functions share each pause. |
| `--fips` | Use the FIPS layer for `v<N>` specs. |
| `--env KEY=VALUE` | An environment variable for all variants. Repeat it for more variables. |

The script invokes all functions at the same time, and it keeps a fixed cadence for the warm
invocations. So a run with `--warm 30 --warm-interval 60` takes at least 30 minutes, for any
number of variants. The functions are `<user>-extperf-<runtime>-<arch>-<variant>`, with `-<k>`
at the end when `--copies` is more than 1. The script keeps them for the next run. If a run
stops with an error, the script still saves the log events that CloudWatch has.

For the time of each step of the extension's own init, add
`--env DD_LOG_LEVEL_BY_TARGET=cold_start_duration=debug`. Only builds that read
`DD_LOG_LEVEL_BY_TARGET` print the steps with this setting. Do not use `DD_LOG_LEVEL=debug` for
a timing run, because the extra logs make the extension slower.

## 3. Read the result

```sh
python3 .claude/skills/measure-extension-layer/scripts/summarize.py /tmp/extension-perf/<label> --compare old new
```

The output has the layer sizes, the medians of each variant, and differences. Each difference
is the difference of two medians, with a bootstrap 95% interval. If the interval contains 0, the
run does not show a difference. Run again with a larger `-n` before you report a change.

How the metrics relate to the release dashboard, Lambda Extension Overhead per Runtime:

| `summarize.py` metric | Dashboard widget |
| --- | --- |
| `init_duration_ms`, variant minus `noext` | Init Duration Overhead |
| `cold_duration_ms` | Duration Overhead. That widget shows the duration of the extension function itself. |
| `warm_duration_ms` or `cold_duration_ms`, variant minus `noext` | Post Runtime Extension Duration. This is an estimate, see below. |
| `each_warm_duration_ms` | The same as `warm_duration_ms`, but each warm invocation is one sample. Use it for a run with few cold starts and many warm invocations. |
| `zipped` | The layer size, `CodeSize` in AWS. |
| `ext.*` | No widget. These are the steps of the extension's init, from the cold start log line. |

The `REPORT` line in CloudWatch has no post-runtime duration. The handler does no work, so
almost all of the difference in duration from `noext` is time after the handler returned. It
also includes any time the extension adds while the handler runs, so it is an upper limit of the
post-runtime duration.

For post-runtime duration, choose the warm interval for the traffic pattern that you check. Use
the setup of the prod self-monitoring functions, `--region us-east-1 --arch x86_64 --runtime
python314`, to get numbers near the dashboard:

| Dashboard traffic | Harness options | Result on 2026-10-19 |
| --- | --- | --- |
| `1min`, one flush in each invocation | `--warm-interval 60` | v101: 89.5 ms. The dashboard showed about 87 ms. |

Other setups gave slower flushes, and the cause is not known. In `us-east-2` on `arm64`, a warm
invocation took about 210 ms with 60 s gaps and about 125 ms with 1 s gaps. In the prod setup with
gaps of about 95 s, it took about 137 ms.

With 60 s between warm invocations, one warm duration had a standard deviation of about 30 ms.
To see a change of about 10 ms, get about 300 warm invocations for each variant, for example
`--copies 8 -n 1 --warm 35 --warm-interval 60`.

Outside the prod setup, do not compare the numbers from this harness with the dashboard
numbers. In all setups, compare variants within one run.

Init duration has a lot of noise. As a guide from earlier runs: with `-n 10`, the interval of
an init duration difference was about 55 ms wide. With `-n 40`, the interval of one median was
about 10 ms wide. Use `-n 40` or more to find a change of about 10 ms.

## 4. Clean up

Delete the functions and the role when you finish the investigation. Run the loop for each
region that you used:

```sh
# A function, not a variable: zsh does not split a variable into words.
awsv() { aws-vault exec sso-serverless-sandbox-account-admin -- aws "$@"; }
REGION=us-east-2
for f in $(awsv lambda list-functions --region $REGION --query "Functions[?starts_with(FunctionName, '<user>-extperf-')].FunctionName" --output text); do
  awsv lambda delete-function --region $REGION --function-name "$f" > /dev/null
done
awsv secretsmanager delete-secret --region $REGION --secret-id <user>-extension-perf-api-key --force-delete-without-recovery
awsv iam delete-role-policy --role-name <user>-extension-perf-role --policy-name read-api-key
awsv iam detach-role-policy --role-name <user>-extension-perf-role --policy-arn arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole
awsv iam delete-role --role-name <user>-extension-perf-role
```

Delete each layer version that you published:

```sh
awsv lambda delete-layer-version --region us-east-2 --layer-name Datadog-Bottlecap-Beta-ARM-<your-name>-perf --version-number <N>
```

`<user>` is your `$USER` in lower case, with each run of other characters changed to `-`.
