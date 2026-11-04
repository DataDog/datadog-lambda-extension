# Buildx provenance spike

Throwaway branch: `lpimentel/spike-buildx-version`. Do not merge this pipeline replacement.

## Run

In GitLab, select **Build > Pipelines > New pipeline**, choose this branch, and run it.
The pipelines API is also supported. Push, schedule, and merge request pipelines
are disabled. There are no build, publish, deploy, or downstream trigger jobs.

Two jobs use the exact bottlecap compile image (`registry.ddbuild.io/images/docker:20.10`)
and its shared runner tags (`arch:amd64` and `arch:arm64`). Each has a three-minute
timeout and no retries. They inspect the existing builder without creating or
bootstrapping a worker. No credentials or Docker configuration contents are printed.

## Evidence to collect

For each architecture, save the job URL and compare:

- The runner pre-build version line with the script's `docker buildx version`.
- Docker client/server versions and client plugin metadata, including plugin path.
- Plugin symlinks and SHA-256 checksums from standard plugin directories.
- Existing builder driver, node status, and reported BuildKit version.

Buildx version is required. Unsupported optional inspection commands print their
exit status and continue; check those results even if the job succeeds.

A plugin path alone does not prove whether the image supplied it or runner setup
replaced it. Compare with the image definition or ask CI Execution to confirm its
pre-build setup before choosing an image change versus a fleet change. This spike
does not upgrade buildx or prove that a dead BuildKit worker caused the hangs.
