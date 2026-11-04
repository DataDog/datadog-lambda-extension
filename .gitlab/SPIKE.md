# Job timeout retry spike

Throwaway branch: `lpimentel/spike-timeout-retry`. Do not merge this pipeline replacement.

## Run

In GitLab, select **Build > Pipelines > New pipeline**, choose this branch, and run it.
The pipelines API is also supported. Push, schedule, and merge request pipelines
are disabled. There are no build, publish, deploy, or downstream trigger jobs.

Both jobs use the bottlecap compile image and shared amd64 runner tag. They sleep
for five minutes with a two-minute job timeout, deliberately failing every attempt.
The only difference between the two jobs is the retry condition.

## Success criteria

List all pipeline jobs with `include_retried=true`, following pagination, then
inspect individual job details for `failure_reason`:

- `retry control`: exactly one attempt, `job_execution_timeout`.
- `retry treatment`: exactly two attempts, both `job_execution_timeout`.
- The pipeline ends failed by design. A green pipeline is not the expected result.

Expected runner usage is about six minutes total plus job setup. A different
failure reason, a canceled job, or a runner-enforced timeout different from two
minutes makes the result inconclusive.

This proves retry behavior only, not the cause of the original BuildKit hangs.
The production follow-up is to add `job_execution_timeout` to the bottlecap retry
list in `.gitlab/templates/pipeline.yaml.tpl`, not to merge this branch.
