---
name: bisect-extension-regression
description: Find the release and then the commit that made the extension's layer size, init duration, duration or post-runtime duration larger. Use it when the release dashboard or a measurement shows a regression between two versions.
---

# Find the change that caused a regression

This skill uses two other skills:

- `check-extension-overhead` gives the numbers of released versions.
- `measure-extension-layer` measures layers in the sandbox account. It has `layer_size.py`,
  `measure.py` and `summarize.py`, which this skill calls.

## 1. Define the regression

Write down these facts before you start:

| Fact | Example |
| --- | --- |
| Metric | Init duration overhead |
| Good version and bad version | `v99` and `v101` |
| Size of the change | +15 ms |
| Runtime, architecture, region, FIPS | Python 3.14, `x86_64`, us-east-1, not FIPS |
| Dashboard stack and traffic, for a dashboard regression | `stack:agent`, `traffic:1min` |

The harness covers only `stack:agent`: its functions have the extension layer, no tracer, and no
traces. For a regression only in `stack:tracing`, the harness cannot show it. Report that, and
give the dashboard numbers.

If you do not know the versions, get them from the time series of the release dashboard with
`check-extension-overhead`.

## 2. Check that the change follows the version, not a date

A change in the dashboard can come from outside the extension, for example from the Datadog
intake or from AWS. For a change that you saw in the dashboard, check when it started before you
measure or build anything. Skip this step for a layer size change, because the dashboard has no
layer size. Also skip it for a change that a harness run showed, because both variants ran at
the same time there. Go to step 3. Query
the metric through the logged-in browser, as in `check-extension-overhead`, in steps of 3 hours.
Compare these series:

| Series | Filter | What it tells you |
| --- | --- | --- |
| The bad version | `env:prod,extension_version:<bad>` | When the change started |
| The good version | `env:prod,extension_version:<good>` | The level before the change |
| Dev functions | `env:dev`, by `extension_version` | Dev functions use dev layers, with their own version numbers. Find a version that did not change at that time. |
| Container functions | `extension_version:N/A`, `runtime:container-rust-alpine` | Not a control. Their image uses the tag `latest-alpine`, so they also get new releases. |

The change comes from the bad version only if both of these are true:

- It starts when the functions change to the bad version.
- It does not start at the same time in functions whose extension did not change.

If not, stop. Report the time of the change and the series that show it. Do not bisect.

Example: post-runtime duration of `traffic:1min` was 76 ms for v99 and 86 ms for v100. But v100
was at 75 to 81 ms for 4 days after its deploy on 2026-09-16. The change started at about
2026-09-20 12:00 UTC. At that time, dev functions on dev version 13 had the same step.

## 3. Reproduce it with the released layers

Measure the good and the bad release in one run, with the runtime, architecture, region and FIPS
mode from step 1:

```sh
aws-vault exec sso-serverless-sandbox-account-admin -- python3 .claude/skills/measure-extension-layer/scripts/measure.py \
  --out /tmp/extension-perf/repro --runtime <runtime> --arch <arch> --region <region> [--fips] \
  -n 20 --warm 3 noext=none good=<good tag> bad=<bad tag>
python3 .claude/skills/measure-extension-layer/scripts/summarize.py /tmp/extension-perf/repro --compare good bad
```

- For a regression that you saw in the dashboard, use the architecture and region of the prod
  self-monitoring functions, `--arch x86_64 --region us-east-1`. Keep the runtime from step 1.
  The harness has `python` (Python 3.13), `python314` (Python 3.14) and `node` (Node.js 24). For a
  regression only in another runtime, report that the harness does not cover it.
- For a FIPS regression, add `--fips`. It changes which released layer `v<N>` means.
- For a size regression, `layer_size.py --arch <arch> [--fips] <good tag> <bad tag>` is enough.
  Size has no noise.
- The tools name the architectures in two ways. `measure.py` takes `x86_64` or `arm64`, as
  Lambda does. `layer_size.py` and the build scripts' `ARCHITECTURE` take `amd64` or `arm64`. So
  for an `x86_64` regression, use `amd64` for those two.

Measure the traffic pattern that changed, and read `each_warm_duration_ms`:

| Dashboard traffic | Harness options | Why |
| --- | --- | --- |
| `traffic:1min` | `--warm 8 --warm-interval 60` | Each invocation flushes after 60 s of idle time. |
| `traffic:busy` | `--warm 60 --warm-interval 1` | The default flush strategy changes after 20 invocations in one environment, to a flush in the background. With 60, most warm invocations come after the change. |

If the 95% interval of `bad minus good` contains 0, run again with `-n 40`. If it still contains
0, stop and report that the harness does not show the regression. Give the numbers. Do not
bisect a change that you cannot measure.

## 4. Find the release

If more than one release is between good and bad, measure the releases between them in one run,
for example `good=v99 mid=v100 bad=v101`. Released layers need no build. Continue with the first
release that shows the regression and the release before it.

## 5. Find the commit

Release tags are on `main`, and `main` has no merge commits. So `git bisect` works between two
tags. First read the list. It can show the cause:

```sh
git log --oneline v100..v101
git diff --stat v100 v101 -- bottlecap/Cargo.lock bottlecap/Cargo.toml
```

For a size regression, look at dependency changes first. For a cold start regression, look at
changes to the init path in `bottlecap/src/bin/bottlecap/main.rs`.

To search, use a separate worktree, so that you do not change the user's checkout. Old commits
do not have these skills, so keep the path of the scripts from the current checkout:

```sh
SKILLS="$PWD/.claude/skills"
git worktree add --detach ../dle-bisect v100
```

The build scripts at an old commit can be different from the current ones. If a build fails,
read `scripts/build_bottlecap_layer.sh` at that commit.

Do not test commits that cannot change the binary. A commit that changes only CI, tests or docs
is not a candidate. A candidate changes at least one input of the build:

- anything in `bottlecap/` except tests, including `bottlecap/build.rs` and `bottlecap/Cargo.*`
- the compile images, `images/Dockerfile.bottlecap*`
- the build scripts, `.gitlab/scripts/compile_bottlecap.sh` and `scripts/build_bottlecap_layer.sh`
- the toolchain and its settings, `rust-toolchain.toml` and `.cargo/`
- the wrapper script in the layer, `scripts/datadog_wrapper`

For example, `8e1bcf9` changes only the compile images, and it embeds the dependency tree in the binary.

### Test many cut points in one round

A build and publish took about 70 s for each commit. A measurement run takes longer, and it can
test many layers at the same time. So build several cut points in one batch, and measure them all
in one run:

1. Choose 3 to 6 cut points that divide the candidates into groups of about the same size. Add
   the good and the bad tag as the two ends, so that all variants come from the same build
   process.
2. Build and publish them. The script writes `<commit> <layer ARN>` lines to `layers.txt` in the
   worktree:

   ```sh
   $SKILLS/bisect-extension-regression/scripts/build_layers.sh ../dle-bisect <your-name>-bisect v99 <cut1> <cut2> <cut3> v100
   ```

   For a FIPS regression, set `FIPS=1` before the command, and use `--fips` for the released layers. Set
   `ARCHITECTURE` and `REGION` in the same way when the regression needs them, with
   `ARCHITECTURE=amd64` for `x86_64`.

3. Measure all of them in one run, with one variant for each line of `layers.txt`, for example
   `good=<arn> c1=<arn> c2=<arn> c3=<arn> bad=<arn>`. Compare each cut point with both ends,
   because the rule below needs both intervals. For example, `--compare good c1 --compare bad c1`
   prints `c1 minus good` and `c1 minus bad`.
4. Put each cut point on one side:
   - It is good if the interval of `cut minus good` contains 0, and the interval of `cut minus
     bad` is below 0.
   - It is bad if the interval of `cut minus bad` contains 0, and the interval of `cut minus good`
     is above 0.
   - Otherwise it is not clear. An interval that contains 0 alone does not show that a cut point
     is near that end. For a +13 ms regression, an interval of [-5, +25] fits both ends.

   The regression is in the group between the last good cut point and the first bad one. If a
   cut point next to that group is not clear, run again with more copies before you go on.
5. Repeat with cut points inside that group, until one commit is left.

For a size regression, build only, and run `layer_size.py --zip` after each build. Size has no
noise, so one cut point at a time is fast enough.

When you finish, remove the worktree.

### Test the cause with a revert

When one commit is left, revert only that commit on top of the bad tag, in the worktree. Do not
push it. Build the good tag, the bad tag and the revert in the same batch, and measure the three
layers in one run. Then all three come from the same build process. This matters most for a size
regression, because a local zip can differ in size from the released one.

```sh
git -C ../dle-bisect checkout --detach <bad tag>
git -C ../dle-bisect revert --no-edit <commit>
REVERT=$(git -C ../dle-bisect rev-parse HEAD)  # the script checks out the tags first, so HEAD moves
$SKILLS/bisect-extension-regression/scripts/build_layers.sh ../dle-bisect <your-name>-revert <good tag> <bad tag> "$REVERT"
```

The commit is the cause if `revert minus bad` shows the regression with the opposite sign, and
the interval of `revert minus good` contains 0. Example: for v98, `revert minus v98` was
-13.7 ms [-20.4, -6.9], against a build of v98. `revert minus v97` was +1.4 ms [-4.8, +7.4],
against the released v97.

## 6. Report

Give these facts:

- The first bad commit, with its PR.
- The numbers that show it: `cut minus good` for the cut points on each side, with intervals and `-n`.
- Or, if step 2 stopped the search, the time of the change and the series that show it.
- The cause, only if you checked it in the code. Say that it is a hypothesis if you did not
  check it.
- The functions and layer versions that you left in the sandbox account. Clean them up as in
  step 4 of `measure-extension-layer`.
