# Buildx client upgrade comparison: do not merge

Branch: `lpimentel/spike-buildx-upgrade`

Base: `df68550e060dd50d6800db07b353440a6013ba44` (local main when created).
The real compile script, Dockerfiles, Rust dependencies, and release profile are
unchanged. This branch replaces the normal GitLab pipeline; it has no publishing,
deployment, or downstream jobs.

## Question

Does changing only the buildx client from fleet v0.8.0 to pinned v0.36.0 change the
frequency or signature of the intermittent bottlecap compile stalls?

The [v0.36.0 release notes](https://github.com/docker/buildx/releases/tag/v0.36.0)
explicitly include "Fix Kubernetes driver builds hanging indefinitely when the
remote exec stream ends" ([#3966](https://github.com/docker/buildx/pull/3966)).
This is a plausible lever, not an established explanation of our failures.
The upstream README allows Docker 19.03+, but compatibility with the actual fleet
BuildKit/Kubernetes setup still needs to be tested.

## Jobs and isolation

Manual `web`/`api` pipelines only, on this exact branch. Each pipeline has eight
jobs: current/upgraded x two replicas x two compile variants:

- amd64, default, AL2
- arm64, FIPS, Alpine

Both arms use `registry.ddbuild.io/images/docker:20.10`, the existing Kubernetes
builder `ci`, and the original compile script. No builder is created, bootstrapped,
removed, or upgraded. Each job gets its own runner-provisioned builder state;
these are matched configurations, not builds forced onto the same physical pod.

The treatment downloads the release binary for the **client's** architecture,
checks a SHA-256 pinned from GitHub release asset metadata, and replaces only
`${DOCKER_CONFIG:-$HOME/.docker}/cli-plugins/docker-buildx` in that job container.
The original plugin is restored on normal/script-failure exit. Job-container death
can prevent cleanup; this does not modify fleet provisioning. Docker credentials,
contexts, and builder configuration are not copied, printed, or replaced.

The wrapper checks original v0.8.0, active expected version, and Kubernetes driver
before compiling. A download, selection, or compatibility failure is a setup
failure, not evidence about compile stalls.

## Bounds and evidence

- Native `retry: 0`. Platform automation can still retry; count attempts separately.
- Compile bounded at 20m, with TERM then KILL after another 30s.
- Whole job timeout 25m, leaving time for installation, preflight, and artifacts.
- `build.log`: full compile output.
- `buildx-spike.log`: original/active versions, builder inspection, compile output,
  and outcome. No environment or credential dump.
- `result.txt`: arm, replica, phase, outcome, exit code, commit/job IDs, and compile
  duration when reached.
- Artifacts retained for two weeks, including failure cases.

Exit 124 records `compile_bound_hit` and fails the job. This is the script's bound,
not GitLab's `job_execution_timeout` failure reason. Exit 137 records
`compile_killed`: it can mean forced termination after the grace period or another
SIGKILL, so it is not by itself proof of a timeout. Other nonzero compile exits
remain failures, including earlier errors that a newer client might surface
instead of hanging.

Expected healthy-run cost is roughly 60-65 runner-minutes per pipeline based on
prior sampler durations, excluding setup. Eight bounded jobs can consume about
160 compile runner-minutes plus overhead. No pipeline has been run yet.

## Run and interpret

1. Review, commit, and push this branch only with approval.
2. Wait until the GitLab mirror branch SHA equals the pushed commit.
3. Trigger one manual pipeline, record its SHA and job IDs, and inspect setup
   compatibility first. Do not silently fall back to the old client.
4. Compare all attempts, including platform retries: setup failures, successes,
   compile errors, bounded stalls, output gaps, and elapsed time per arm/variant.
5. Repeat only with an agreed runner budget. Four attempts per arm in one pipeline
   are a compatibility check and small sample, not evidence that hangs are fixed.

A newer client failing promptly after stream loss could be useful even if the
underlying worker problem remains. Neither all-green runs nor a change in failure
shape proves a worker died; cluster events would still be needed for attribution.

## Local verification

```sh
bash -n .gitlab/scripts/compare_buildx.sh
shellcheck .gitlab/scripts/compare_buildx.sh
python3 .gitlab/scripts/test_compare_buildx.py -v
```

The wrapper tests fake Docker/download commands and mock successful checksum
verification. They cover both client architectures, plugin restoration, preserved
credentials/builder sentinel files, failure propagation, and timeout artifacts.
They do not exercise Docker plugin loading, Kubernetes, or actual Rust compilation.
GitLab lint validates the pipeline separately; local validation is not a CI result.

Preparation checks passed: Bash syntax, ShellCheck, YAML lint, all 11 wrapper tests,
pipeline structure assertions, and `git diff --check`. GitLab's project CI lint
returned `valid: true`, with no errors or warnings. The real compile script,
Dockerfiles, and Rust code have no diff against the base. No CI run has occurred.
