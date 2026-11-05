#!/bin/bash
# Throwaway client comparison. Do not merge.
set -euo pipefail

exec > >(tee buildx-spike.log) 2>&1
: >build.log
phase=setup
scratch=
plugin=
replaced=0
compile_started=0
version=v0.36.0

finish() {
    local rc=$? outcome
    trap - EXIT
    set +e
    if [ "$phase" != compile ]; then
        outcome=setup_failure
    elif [ "$rc" -eq 124 ]; then
        outcome=compile_bound_hit
    elif [ "$rc" -eq 137 ]; then
        outcome=compile_killed
    elif [ "$rc" -eq 0 ]; then
        outcome=success
    else
        outcome=compile_failure
    fi
    {
        printf 'arm=%s\nreplica=%s\n' "${BUILDX_ARM:-unset}" "${REPLICA:-unset}"
        printf 'phase=%s\noutcome=%s\nexit_code=%s\n' "$phase" "$outcome" "$rc"
        printf 'commit=%s\njob_id=%s\n' "${CI_COMMIT_SHA:-local}" "${CI_JOB_ID:-local}"
        if [ "$compile_started" -ne 0 ]; then
            printf 'compile_elapsed_seconds=%s\n' "$(( $(date +%s) - compile_started ))"
        fi
    } | tee result.txt
    if [ "$replaced" -eq 1 ]; then
        mv -f "$scratch/original" "$plugin" || rc=1
    fi
    if [ -n "$scratch" ]; then
        rm -rf "$scratch"
    fi
    exit "$rc"
}
trap finish EXIT

printf 'buildx spike: %s, arm=%s, replica=%s\n' "$(date -u +%FT%TZ)" "${BUILDX_ARM:-unset}" "${REPLICA:-unset}"
docker --version
original_version=$(timeout -k 5s 30s docker buildx version)
echo "Original client: $original_version"
# Fail rather than silently changing the control when fleet provisioning changes.
case "$original_version" in
    *" v0.8.0 "*) ;;
    *) echo 'Expected fleet buildx v0.8.0; reassess the baseline before running'; exit 1 ;;
esac

case "${BUILDX_ARM:-}" in
    current) expected=v0.8.0 ;;
    upgraded)
        expected=$version
        case "$(uname -m)" in
            x86_64)
                client_arch=amd64
                checksum=07823fdfcd82a41be90155a8b16876c1a780a6462de805a9f3f63b3119ccfb99
                ;;
            aarch64)
                client_arch=arm64
                checksum=70382de03915c07c488ae4ddc4f7e169ee978f953e754ecfce110ba017e0132b
                ;;
            *) echo 'Unsupported client architecture'; exit 1 ;;
        esac
        scratch=$(mktemp -d)
        plugin="${DOCKER_CONFIG:-$HOME/.docker}/cli-plugins/docker-buildx"
        # Preserve Docker credentials, contexts and builder state. Replace only
        # the job container's plugin, then restore it on exit.
        test -f "$plugin"
        cp -P "$plugin" "$scratch/original"
        # The fleet image has no wget. Provision curl in this job only if needed.
        if ! command -v curl >/dev/null 2>&1; then
            timeout -k 5s 90s apt-get update -qq
            timeout -k 5s 90s apt-get install -y --no-install-recommends curl ca-certificates
        fi
        timeout -k 5s 180s curl --fail --silent --show-error --location \
            --connect-timeout 15 --max-time 150 --output "$scratch/docker-buildx" \
            "https://github.com/docker/buildx/releases/download/$version/buildx-$version.linux-$client_arch"
        printf '%s  %s\n' "$checksum" "$scratch/docker-buildx" | sha256sum -c -
        chmod +x "$scratch/docker-buildx"
        mv -f "$scratch/docker-buildx" "$plugin"
        replaced=1
        ;;
    *) echo 'Set BUILDX_ARM=current or upgraded'; exit 1 ;;
esac

active_version=$(timeout -k 5s 30s docker buildx version)
echo "Active client: $active_version"
case "$active_version" in
    *" $expected "*) ;;
    *) echo 'Docker did not select the expected plugin'; exit 1 ;;
esac

# Keep the same named builder. Do not create or bootstrap a new pool.
export BUILDX_BUILDER=ci
inspection=$(timeout -k 5s 45s docker buildx inspect ci)
printf '%s\n' "$inspection"
if ! printf '%s\n' "$inspection" | grep -Eq '^Driver:[[:space:]]+kubernetes[[:space:]]*$'; then
    echo 'Expected the existing Kubernetes builder ci'; exit 1
fi

phase=compile
compile_started=$(date +%s)
echo "Compile start: $(date -u +%FT%TZ)"
# An internal bound leaves time for artifacts, rather than losing logs to the
# job's 30m limit. Preserve failure/timeout exits, unlike the earlier sampler.
# Test overrides exercise this wrapper without Docker or compiling Rust.
timeout -k 30s -s TERM "${SPIKE_BUILD_LIMIT:-20m}" \
    "${SPIKE_COMPILE_SCRIPT:-.gitlab/scripts/compile_bottlecap.sh}" 2>&1 | tee build.log
