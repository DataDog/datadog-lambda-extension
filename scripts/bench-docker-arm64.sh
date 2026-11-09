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
# with the host baselines, with the same baseline-<name>.txt note as `bench-compare.sh save`
# (machine, image and limits, load, profile, commit). As there, older results saved as <name>
# are removed first. Compare them on the host with
#   CARGO_TARGET_DIR=bottlecap/target-linux-arm64 scripts/bench-compare.sh compare <old> <new>
#
# The Docker host must be arm64. On an x86 host Docker emulates arm64 (QEMU, binfmt), and the
# numbers measure the emulator, so the script stops.
#
# Environment:
#   RUST_IMAGE   docker image (default rust:1.93-bookworm; keep it on the stable toolchain CI
#                uses). bottlecap needs protoc for the build script: the image installs it.
#   DOCKER_CPUS  CPU limit for the container (default 2, like a Lambda sandbox's 2 vCPUs. Lambda
#                at 128 MB gets about 0.07 vCPU of that, so absolute numbers here are still
#                faster than production; the ratios between benches are what transfer).
#   DOCKER_MEM   memory limit (default 2g; the bench binaries hold several MB of fixtures).
#   BENCH_FAST=1 no LTO, 16 codegen units; fast link, NOT the production profile.
#   BENCH_ALLOW_EMULATION=1
#                run on an x86 Docker host anyway, to check that the suite runs. The note
#                marks the results as emulated; never use them as a baseline.
set -euo pipefail

name="${1:-}"
[[ -n "$name" ]] || { sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1; }
shift
# The name is a directory under every bench ID, and the container deletes the old ones.
name_re='^[A-Za-z0-9][A-Za-z0-9._-]*$'
[[ "$name" =~ $name_re ]] || { echo "baseline name must match $name_re: $name" >&2; exit 1; }

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

# HEAD, plus a hash of the uncommitted diff when the tree has changes, as `bench-compare.sh`
# records it.
git_rev="$(git -C "$here" rev-parse --short HEAD 2>/dev/null)" || git_rev=unknown
if [[ "$git_rev" != unknown && -n "$(git -C "$here" status --porcelain --untracked-files=no)" ]]; then
  git_rev="$git_rev-dirty (diff $(git -C "$here" diff HEAD | git hash-object --stdin | cut -c1-12))"
fi

# The daemon's kernel: an x86_64 one runs linux/arm64 under emulation.
docker_arch="$(docker info --format '{{.Architecture}}')"
emulated=""
case "$docker_arch" in
  aarch64 | arm64) ;;
  *)
    if [[ "${BENCH_ALLOW_EMULATION:-0}" != 1 ]]; then
      echo "the Docker host is $docker_arch, so linux/arm64 runs under emulation and the numbers" >&2
      echo "measure the emulator. Run on an arm64 host (Apple Silicon, Graviton), or set" >&2
      echo "BENCH_ALLOW_EMULATION=1 to check only that the suite runs." >&2
      exit 1
    fi
    emulated=", EMULATED on $docker_arch (NOT a baseline)"
    ;;
esac

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
  -e BENCH_NAME="$name" \
  -e BENCH_GIT="$git_rev" \
  -e BENCH_CONTAINER="$image --cpus $cpus --memory $mem$emulated" \
  ${docker_flags[@]+"${docker_flags[@]}"} \
  "$image" \
  bash -c "
    set -euo pipefail
    if ! command -v protoc >/dev/null; then
      apt-get update -qq && apt-get install -y -qq --no-install-recommends protobuf-compiler >/dev/null
    fi
    cpu=\$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //' || true)
    overrides=\$(env | grep '^CARGO_PROFILE_BENCH_' | sort | tr '\n' ' ' || true)
    profile=\${overrides:+bench with \${overrides}(NOT production)}
    note=\"machine: \$(uname -sm), \$(nproc) cpus visible\${cpu:+, \$cpu}; container: \$BENCH_CONTAINER; load average: \$(cut -d' ' -f1-3 /proc/loadavg); profile: \${profile:-bench = release (opt-level z, lto, codegen-units 1)}\"
    echo \"\$note\"
    results=\"\$CARGO_TARGET_DIR/criterion\"
    if [[ -d \"\$results\" ]]; then
      find \"\$results\" -type f -path \"*/\$BENCH_NAME/benchmark.json\" | sed 's|/benchmark.json\$||' |
        while IFS= read -r d; do rm -rf \"\$d\"; done
      rm -f \"\$results/baseline-\$BENCH_NAME.txt\"
    fi
    cargo bench \"\$@\"
    mkdir -p \"\$CARGO_TARGET_DIR/criterion\"
    printf '%s\nsaved: %s\ngit: %s\nargs: %s\n' \"\$note\" \"\$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \"\$BENCH_GIT\" \"\$*\" \\
      > \"\$CARGO_TARGET_DIR/criterion/baseline-\$BENCH_NAME.txt\"
  " bench "${cargo_args[@]}"
echo "saved baseline '$name' under bottlecap/target-linux-arm64/criterion (notes in baseline-$name.txt)"
