# Job timeout retry spike

Throwaway branch: `lpimentel/spike-timeout-retry`. Do not merge this pipeline replacement.

## Run

In GitLab, select **Build > Pipelines > New pipeline**, choose this branch, and run it.
The pipelines API is also supported. Push, schedule, and merge request pipelines
are disabled. There are no build, publish, deploy, or downstream trigger jobs.

All three jobs use the bottlecap compile image and shared amd64 runner tag. They
sleep for five minutes with a two-minute job timeout, deliberately failing every
attempt. Only their retry settings differ.

## First run

[Pipeline 150273229](https://gitlab.ddbuild.io/DataDog/datadog-lambda-extension/-/pipelines/150273229)
ran the original two arms. Both produced two attempts with `job_execution_timeout`,
contrary to the expectation that only the treatment would retry. Lucas confirmed
neither job was manually retried. This does not demonstrate a benefit from adding
`job_execution_timeout` to the production retry list.

## Three-arm follow-up

List all pipeline jobs with `include_retried=true`, following pagination, then
inspect individual job details for `failure_reason`:

| Job | Retry setting | What to check |
|---|---|---|
| `retry control` | `max: 1`, `when: [stuck_or_timeout_failure]` | Whether the first run's two attempts recur |
| `retry treatment` | `max: 1`, `when: [job_execution_timeout]` | Whether the first run's two attempts recur |
| `retry disabled` | `retry: 0` | Exactly one attempt if configured retry limits are respected |

Every attempt should fail with `job_execution_timeout`. The pipeline ends failed
by design; a green pipeline is not the expected result. Confirm nobody manually
retried a job before interpreting extra attempts as automation.

If `retry disabled` also gets a second attempt, the explicit no-retry setting is
not controlling the observed behavior. Investigate effective configuration and
platform automation. If it stays at one attempt while the other arms retry, the
cause still needs explanation; that alone does not prove how conditions match.

If the first two arms again run twice and the disabled arm runs once, expect about
ten minutes of aggregate runner usage plus setup. A different failure reason, a
canceled job, or a runner-enforced timeout different from two minutes makes the
comparison inconclusive.

This investigates retry behavior only, not the cause of the original BuildKit
hangs. Hold the production retry-list change until the unexpected control retry
is explained. Do not merge this pipeline replacement.
