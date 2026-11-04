#!/bin/bash

# Throwaway CI diagnostic: identify which BuildKit node serves a build, and
# whether the requested platform is native there.
# Branch lpimentel/spike-compile-sampler. Do not merge.
#
# Builds a two-step image a few times for TARGET_PLATFORM and prints the
# identity visible from inside the build container, plus the builder pool before
# and after every build. The Dockerfile base is the image the compile jobs
# already use, so the pulls are cheap.
#
# Identity is whatever the driver exposes: build containers report the fixed
# hostname buildkitsandbox, so the useful signals are the architecture, the
# cgroup path, pod mounts, and whether the pool grows a node for the platform.
# Resolving a pod to a pod name needs cluster access and is not attempted.

set -u

TARGET_PLATFORM=${TARGET_PLATFORM:-amd64}
LOG=node-probe.log
: >"$LOG"

note() { echo "### $*" | tee -a "$LOG"; }

mkdir -p /tmp/probe
cat >/tmp/probe/Dockerfile <<'DOCKERFILE'
FROM registry.ddbuild.io/images/docker:20.10
RUN echo "identity-1 arch=$(uname -m) nproc=$(nproc)" \
 && head -2 /proc/cpuinfo \
 && cat /etc/hostname \
 && (cat /etc/hosts || true) \
 && head -1 /proc/self/cgroup \
 && (grep -o 'pods/[^/]*' /proc/self/mountinfo | sort -u | head -3 || true)
RUN echo "identity-2 arch=$(uname -m)"
DOCKERFILE

DEBUG_PREFIX=""
if docker buildx --help 2>&1 | grep -q -- '--debug'; then
    DEBUG_PREFIX="--debug"
fi

note "client hostname $(hostname), client arch $(uname -m), requesting linux/${TARGET_PLATFORM}"
note "buildx root --debug: ${DEBUG_PREFIX:-unsupported}"

pool_summary() {
    docker buildx ls 2>&1 | grep -oE 'buildkit-platform-[a-z0-9-]+ +[a-z]+' | awk '{printf "%s(%s) ", $1, $2}'
    echo
}

note "pool before: $(pool_summary)"

for attempt in 1 2 3; do
    note "build ${attempt} for linux/${TARGET_PLATFORM}"
    if [ "$attempt" = "3" ] && [ -n "$DEBUG_PREFIX" ]; then
        docker buildx "$DEBUG_PREFIX" build --platform "linux/${TARGET_PLATFORM}" --progress=plain -f /tmp/probe/Dockerfile /tmp/probe >"build-${attempt}.log" 2>&1
    else
        docker buildx build --platform "linux/${TARGET_PLATFORM}" --progress=plain -f /tmp/probe/Dockerfile /tmp/probe >"build-${attempt}.log" 2>&1
    fi
    rc=$?
    echo "build ${attempt} exit ${rc}" | tee -a "$LOG"
    grep -E 'identity-[12]|cpuinfo|model name|^[0-9a-f]{12}|exec format|no match' "build-${attempt}.log" | tail -14 | tee -a "$LOG"
    if [ -n "$DEBUG_PREFIX" ] && [ "$attempt" = "3" ]; then
        note "node-related lines from the debug build"
        grep -iE 'node|pod|load balanc|dial|endpoint|worker' "build-${attempt}.log" | tail -25 | tee -a "$LOG"
    fi
    note "pool after build ${attempt}: $(pool_summary)"
done

note "probe finished"
