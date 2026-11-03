#!/usr/bin/env bash
# Runs the bottlecap criterion suite on Linux arm64 inside Docker, the platform the extension
# ships on. Lambda arm64 is Graviton; an Apple Silicon host runs the arm64 image natively, so
# this is the closest local stand-in and the only way to run the Linux-only `/proc` benches.
#
# Usage:
#   scripts/bench-docker-arm64.sh <baseline name> [bench...] [-- <criterion args>]
#
# Examples:
#   scripts/bench-docker-arm64.sh linux-main
#   scripts/bench-docker-arm64.sh linux-main enhanced_metrics -- --quick
#
# Results land in bottlecap/target-linux-arm64/criterion/<bench>/<name>/, so they never mix
# with the host baselines. Compare them on the host with
#   CARGO_TARGET_DIR=bottlecap/target-linux-arm64 scripts/bench-compare.sh compare <old> <new>
#
# Environment:
#   RUST_IMAGE   docker image (default rust:1.93-bookworm; keep it on the toolchain in rust-toolchain
#                or CI uses). bottlecap needs protoc for the build script: the image installs it.
#   DOCKER_CPUS  CPU limit for the container (default 2, like a Lambda sandbox's 2 vCPUs. Lambda
#                at 128 MB gets about 0.07 vCPU of that, so absolute numbers here are still
#                faster than production; the ratios between benches are what transfer).
#   DOCKER_MEM   memory limit (default 2g; the bench binaries hold several MB of fixtures).
#   BENCH_FAST=1 no LTO, 16 codegen units; fast link, NOT the production profile.
set -euo pipefail

name="${1:-}"
[[ -n "$name" ]] || { sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1; }
shift

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="${RUST_IMAGE:-rust:1.93-bookworm}"
cpus="${DOCKER_CPUS:-2}"
mem="${DOCKER_MEM:-2g}"

benches=()
extra=()
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "--" ]]; then
    shift
    extra=("$@")
    break
  fi
  benches+=("--bench" "$1")
  shift
done

# `${arr[@]+"${arr[@]}"}` expands to nothing when the array is empty; a bare "${arr[@]}" is an
# unbound variable under `set -u` on bash 3.2 (/bin/bash on macOS). The cargo arguments reach
# the container shell as positional parameters, not as script text, so filters with spaces or
# shell metacharacters pass through unchanged.
cargo_args=(${benches[@]+"${benches[@]}"} -- --save-baseline "$name" ${extra[@]+"${extra[@]}"})

# Optional docker flags.
docker_flags=()
if [[ "${BENCH_FAST:-0}" == 1 ]]; then
  docker_flags+=(-e CARGO_PROFILE_BENCH_LTO=false -e CARGO_PROFILE_BENCH_CODEGEN_UNITS=16)
fi
if [[ -t 0 ]]; then
  docker_flags+=(-it)
fi

# Named volumes keep the registry and git checkouts between runs.
docker run --rm \
  --platform linux/arm64 \
  --cpus "$cpus" --memory "$mem" \
  -v "$here:/src" \
  -v bottlecap-bench-cargo-registry:/usr/local/cargo/registry \
  -v bottlecap-bench-cargo-git:/usr/local/cargo/git \
  -w /src/bottlecap \
  -e CARGO_TARGET_DIR=/src/bottlecap/target-linux-arm64 \
  -e CARGO_NET_GIT_FETCH_WITH_CLI=true \
  ${docker_flags[@]+"${docker_flags[@]}"} \
  "$image" \
  bash -c "
    set -euo pipefail
    if ! command -v protoc >/dev/null; then
      apt-get update -qq && apt-get install -y -qq --no-install-recommends protobuf-compiler >/dev/null
    fi
    echo 'machine:' \$(uname -sm) \$(nproc) 'cpus visible,' \$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- || true)
    echo 'profile:' \${CARGO_PROFILE_BENCH_LTO:+bench FAST (no LTO, NOT production)}\${CARGO_PROFILE_BENCH_LTO:-bench = release (opt-level z, lto, codegen-units 1)}
    cargo bench \"\$@\"
  " bench "${cargo_args[@]}"
echo "saved baseline '$name' under bottlecap/target-linux-arm64/criterion"
