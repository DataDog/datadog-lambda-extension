# Compile stall sampler spike

Throwaway branch: `lpimentel/spike-compile-sampler`. Do not merge this pipeline
replacement.

## What it is for

The `bottlecap` compile jobs occasionally stop producing output and are killed at
the 20m job timeout. Traces show output stopping mid-step (step-elapsed 7.8s,
10.1s, or 150.9s) after a dense burst, followed by 16-18 minutes of nothing. That
rules out a merely slow phase, but it does not say whether the build client is
waiting on something that died, or whether the step is starved of CPU, memory, or
IO.

This spike runs the real compile unchanged and samples the job container while it
runs, so a stall is captured with resource and network state attached.

## BuildKit node probe

Runs on every triggered pipeline, on both `arch:amd64` and `arch:arm64` runners,
from `.gitlab/scripts/probe_buildkit_node.sh`. It builds a two-step image three
times and records the identity visible from inside the build container:
hostname, `/proc/self/cgroup`, pod mounts, `uname -m`, and the builder pool
before and after. The two `RUN` steps also show whether the serving node changes
partway through a single build.

What it is for: `buildx ls` shows builder `ci` as a pool of pods named
`buildkit-platform-amd64-<hash>-<suffix>`, and the pool differs per job, so which
node serves a given build is otherwise invisible. Knowing the serving
architecture also tests whether `linux/arm64` builds are emulated on
`linux/amd64` nodes, which would explain why the arm64 variants are both the
slowest and the most likely to time out.

A pod UID is not a pod name. Resolving one to the other needs cluster access, so
the probe gives CI Execution something exact to look up rather than an answer by
itself.

## Sampler arms

Two arms, two replicas each, from `.gitlab/scripts/sample_compile.sh`. These only
run when the pipeline is triggered with `RUN_SAMPLER=1`:

| Arm | Why this variant |
|---|---|
| `arm64, fips, alpine`, `parallel: 2` | Slowest variant and the highest historical timeout count. Stalls on this Dockerfile path were traced inside the final compile step. |
| `amd64`, `parallel: 2` | Fastest variant. This Dockerfile path is where two traced stalls stopped at a step boundary (main build never started). |

The compile script, image, runner tags, and build arguments are unchanged, so the
build is the production build. Only the sampling wrapper is new.

## How to run

In GitLab, select **Build > Pipelines > New pipeline**, choose this branch, and
run it. The pipelines API also works. Push, schedule, and merge request pipelines
are disabled, and there are no build, publish, deploy, or downstream trigger jobs.

Run the probe alone by triggering without `RUN_SAMPLER`. Add `RUN_SAMPLER=1` to
run the sampler arms in the same pipeline.

## Bounds

| Setting | Value |
|---|---|
| Compile bound (`timeout -s TERM`) | 25m |
| Job timeout | 30m |
| Sample interval | 10s |
| Silence threshold for a detailed snapshot | 120s |
| Minimum spacing between detailed snapshots | 180s |
| Retry | `retry: 0` |

`retry: 0` does not guarantee a single attempt: the retry spike showed platform
automation can add one. If a second attempt appears, treat it as a second sample.

The script exits 0 when the 25m compile bound is hit, because the timeout is the
observation. It exits non-zero only for a real compile failure. The build bound
is inside the job timeout so the process exits cleanly and artifacts upload.

Expected cost: four jobs, roughly 5-11 minutes each if the build completes
(about 32 runner-minutes), or up to 25 minutes each if a stall is caught.

## What to read

Per job: `sampler.log` and `build.log` in the artifacts, and the same sampling
output inline in the job trace.

Sampled every 10s: node load, memory and swap, cumulative TCP counters and
retransmissions, TCP state counts, socket summary, and container processes. On a
stall, in addition: last write time of `build.log`, tail of the build output,
`ss` details per connection, process table, live `docker buildx ls` and
`docker buildx inspect ci` output, and disk usage.

`/proc` is not namespaced for load, memory, socket, and TCP counters, so those
describe the runner node. The build itself runs in a separate BuildKit pod, so
container-level CPU or memory numbers describe the client, not the builder.

## Interpreting a captured stall

| Observation at the stall | Reading |
|---|---|
| Client idle, established connection to the builder or API server present, retransmissions climbing | The client is blocked on a connection that is not making progress. Points at the BuildKit session or the network path, not at the build itself. |
| `docker buildx inspect ci` shows nodes missing or a dead endpoint | Worker loss during the build, matching the DEVX BuildKit note. |
| No established connection at all, no sockets for the buildx process | Client already lost the session; check the last build output line for the phase. |
| Node load or memory pressure high (load average above core count, swap in use, PSI high) | The step really is resource-starved. Then the fix is resources or caching, not the buildx client. |
| CPU counters climbing, step slow but producing output | Not a stall: a slow phase. Compare against a successful job of the same variant. |

False triggers are possible: a healthy build can be quiet for a moment, in which
case the detailed block just shows a healthy node. Nothing about the spike
identifies the owner of the BuildKit plugin path or the supported client version;
that question goes to CI Execution.

## Cleanup

Delete the branch when done. Do not merge this pipeline replacement.
