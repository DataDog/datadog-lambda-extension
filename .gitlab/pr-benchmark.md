# PR performance measurements

Opening a same-repository GitHub PR or pushing another commit to it dispatches one
benchmark publication after `layer (amd64)` and its existing size check succeed.
Here `external_pull_request_event` means GitHub-hosted PRs, including employee PRs.
The companion branch-push pipeline does not dispatch again. Fork PRs are excluded;
repository identity is not an employee-status check.

The generated build pipeline starts a detached child pipeline without a dependent
or mirror strategy. It downloads the existing layer artifact by the exact build
pipeline ID; no extra Rust compilation is added. Both dispatch and publication
allow failure. Existing build, test, release, and E2E gates keep their behavior.
Publication and AWS measurement batches do not hold up the PR pipeline.

The publisher verifies the source SHA against the build pipeline, resolves the
open PR through GitHub, and discovers the deployed extension FIFO queue and managed
layer name from `PrBenchmark-dev-Shared` in `ca-central-1`. It pairs the retained
candidate extension with the released `Datadog-Node22-x` layer, recording both
Node package versions. Messages use schema 2 and `name:lambda-extension-pr-<number>`.
The shared stack controls retention (currently 14 days); the benchmark pool cleans
expired managed versions after active batches drain. Existing integration-layer
cleanup does not touch this managed layer name.

## Rollout

The publisher reuses `sandbox-publish-externalid` and `sandbox-layer-deployer`
through the repository's existing Vault authentication script. The role needs the
shared stack's `PublisherPolicyArn` policy for queue sends, managed-layer publication,
released-layer reads, and stack-output discovery. No new Vault secret is needed.
Confirm those permissions and run the real GitLab artifact-to-queue flow before
merging. This draft does not create or depend on a separate benchmark GitLab project.

Run the publisher tests with `node --test .gitlab/scripts/tests/pr-benchmark.test.js`.
Publication manifests are retained as CI artifacts for 14 days. A failed publication
can miss a measurement; retries within SQS's deduplication window do not enqueue the
same PR/SHA/released-Node combination twice.
