#!/bin/bash

# Throwaway CI diagnostic: identify which BuildKit node serves a build.
# Branch lpimentel/spike-compile-sampler. Do not merge.
#
# Builds a two-step image a few times and prints the identity visible from
# inside the build container, plus the builder pool before and after. The
# Dockerfile base is the image the compile jobs already use, so the pulls are
# cheap. The identity bundle is what it is: pod hostnames and pod UIDs are
# whatever the Kubernetes driver actually exposes, and resolving a pod UID to a
# pod name needs cluster access.

set -u

LOG=node-probe.log
: >"$LOG"

note() { echo "### $*" | tee -a "$LOG"; }

mkdir -p /tmp/probe
cat >/tmp/probe/Dockerfile <<'DOCKERFILE'
FROM registry.ddbuild.io/images/docker:20.10
RUN echo "identity-1 arch=$(uname -m) hostname=$(hostname)" \
 && cat /etc/hostname \
 && head -1 /proc/self/cgroup \
 && (grep -o 'pods/[0-9a-f-]*' /proc/self/mountinfo | sort -u | head -3 || true) \
 && (head -3 /etc/resolv.conf || true)
RUN echo "identity-2 arch=$(uname -m) hostname=$(hostname)"
DOCKERFILE

note "client hostname $(hostname), client arch $(uname -m)"
note "builder pool before the probe"
docker buildx ls 2>&1 | tee -a "$LOG"
docker buildx inspect ci 2>&1 | grep -E 'Name:|Status:' | tee -a "$LOG"

DEBUG_FLAG=""
if docker buildx build --help 2>&1 | grep -q -- '--debug'; then
    DEBUG_FLAG="--debug"
fi
note "buildx --debug support: ${DEBUG_FLAG:-none}"

for attempt in 1 2 3; do
    note "build ${attempt}"
    # shellcheck disable=SC2086
    docker buildx build $DEBUG_FLAG --progress=plain -f /tmp/probe/Dockerfile /tmp/probe >"build-${attempt}.log" 2>&1
    rc=$?
    echo "build ${attempt} exit ${rc}" | tee -a "$LOG"
    grep -E 'identity-[12]|pods/[0-9a-f-]|^[0-9]+:[a-z_]+:' "build-${attempt}.log" | tail -12 | tee -a "$LOG"
    if [ -n "$DEBUG_FLAG" ]; then
        note "node-related debug lines for build ${attempt}"
        grep -iE 'node|pod|load balanc|dial|endpoint|builder' "build-${attempt}.log" | tail -25 | tee -a "$LOG"
    fi
done

note "builder pool after the probe"
docker buildx ls 2>&1 | tee -a "$LOG"
note "probe finished"
