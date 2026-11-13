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
# (scripts/bench-note.sh), plus the image and the container limits. As there, older results
# saved as <name> are removed first. Compare them on the host with
#   CARGO_TARGET_DIR=bottlecap/target-linux-arm64 scripts/bench-compare.sh compare <old> <new>
#
# The Docker host must be arm64. On an x86 host Docker emulates arm64 (QEMU, binfmt), and the
# numbers measure the emulator, so the script stops.
#
# Environment:
#   RUST_IMAGE   docker image (default rust:1.93-bookworm; keep it on the stable toolchain CI
#                uses). It must be Debian-based: the script runs bash in it, and bottlecap needs
#                protoc for the build script, which the script installs with apt-get. The script
#                stops before the run if the image has no bash, or neither protoc nor apt-get.
#   DOCKER_CPUS  CPU limit for the container (default 2, like a Lambda sandbox's 2 vCPUs. Lambda
#                at 128 MB gets about 0.07 vCPU of that, so absolute numbers here are still
#                faster than production; the ratios between benches are what transfer).
#   DOCKER_MEM   memory limit (default 2g; the bench binaries hold several MB of fixtures).
#   BENCH_FAST=1 no LTO, 16 codegen units; fast link, NOT the production profile.
#   BENCH_ALLOW_EMULATION=1
#                run on an x86 Docker host anyway, to check that the suite runs. The note
#                marks the results as emulated; never use them as a baseline.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/bench-note.sh
. "$here/scripts/bench-note.sh"

name="${1:-}"
[[ -n "$name" ]] || { sed -n '2,34p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1; }
shift
bench_check_name "$name"

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

# The container runs bash, and apt-get when the image has no protoc. Check before the long run.
if ! docker run --rm --platform linux/arm64 "$image" sh -c \
  'command -v bash && { command -v protoc || command -v apt-get; }' >/dev/null; then
  echo "cannot run bash, or apt-get to install protoc, in $image. RUST_IMAGE must be a" >&2
  echo "Debian-based Rust image, for example rust:1.93-bookworm." >&2
  exit 1
fi

# Optional docker flags.
docker_flags=()
if [[ "${BENCH_FAST:-0}" == 1 ]]; then
  docker_flags+=(-e CARGO_PROFILE_BENCH_LTO=false -e CARGO_PROFILE_BENCH_CODEGEN_UNITS=16)
fi
if [[ -t 0 ]]; then
  docker_flags+=(-it)
fi

# Before the run, as `bench-compare.sh save` records it.
git_rev="$(bench_git_rev "$here")"
target=bottlecap/target-linux-arm64

# Named volumes keep the registry and git checkouts between runs. The container sources the
# note helper from the bind mount, so the note is the one `bench-compare.sh save` writes.
# The script runs in the container and expands its own variables.
# shellcheck disable=SC2016
docker run --rm \
  --platform linux/arm64 \
  --cpus "$cpus" --memory "$mem" \
  -v "$here:/src" \
  -v bottlecap-bench-cargo-registry:/usr/local/cargo/registry \
  -v bottlecap-bench-cargo-git:/usr/local/cargo/git \
  -w /src/bottlecap \
  -e CARGO_TARGET_DIR="/src/$target" \
  -e CARGO_NET_GIT_FETCH_WITH_CLI=true \
  -e BENCH_NAME="$name" \
  -e BENCH_GIT="$git_rev" \
  -e BENCH_CONTAINER="$image --cpus $cpus --memory $mem$emulated" \
  ${docker_flags[@]+"${docker_flags[@]}"} \
  "$image" \
  bash -c '
    set -euo pipefail
    . /src/scripts/bench-note.sh
    if ! command -v protoc >/dev/null; then
      apt-get update -qq && apt-get install -y -qq --no-install-recommends protobuf-compiler >/dev/null
    fi
    results="$CARGO_TARGET_DIR/criterion"
    note="$(bench_machine_note)"
    echo "$note"
    bench_clear_baseline "$results" "$BENCH_NAME"
    cargo bench "$@"
    bench_write_note "$results" "$BENCH_NAME" "$note" "$BENCH_GIT" "$(bench_quote_args "$@")"
  ' bench "${cargo_args[@]}"
# The bind mount lets cargo see an edit made during the run. The results belong to the
# container user (root on a Linux host), so a second container marks the note.
# shellcheck disable=SC2016
bench_check_after_run "$here" "$git_rev" \
  docker run --rm --platform linux/arm64 -v "$here:/src" "$image" \
  bash -c '. /src/scripts/bench-note.sh && bench_mark_changed "$@"' bench \
  "$(bench_note_file "/src/$target/criterion" "$name")"
echo "saved baseline '$name' under $target/criterion (notes in baseline-$name.txt)"
