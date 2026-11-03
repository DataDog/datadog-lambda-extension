#!/usr/bin/env bash
# Runs the bottlecap criterion suite with the production profile, saves the results as a named
# baseline, and compares it with another baseline.
#
# Usage:
#   scripts/bench-compare.sh save <name> [bench...] [-- <criterion args>]
#       Run the suite (or only the named benches: logs, traces, invocation, enhanced_metrics,
#       config, init) and save the baseline <name>.
#   scripts/bench-compare.sh compare <old> <new> [critcmp args]
#       Print a table of <old> vs <new>. Extra args go to critcmp (for example `-t 5` hides
#       differences of 5% or less, `-f 'logs/.*'` filters by regex).
#   scripts/bench-compare.sh export <name> <file.json>
#       Export a baseline to one JSON file, so it survives `cargo clean` and can be compared
#       from another machine with `critcmp old.json new.json`.
#   scripts/bench-compare.sh list
#       List the baselines critcmp knows about.
#
# Environment:
#   CARGO_TARGET_DIR   where cargo builds and where criterion keeps baselines
#                      (default: bottlecap/target). critcmp reads the same directory.
#   BENCH_FAST=1       build the bench profile without LTO and with 16 codegen units. Fast to
#                      link, but NOT the production profile; use it only to check that a bench
#                      runs, never for a baseline.
#   CRITCMP            path to critcmp (default: critcmp on PATH). Install with `cargo install critcmp`.
#
# The bench profile inherits [profile.release] from bottlecap/Cargo.toml: opt-level "z", lto,
# codegen-units 1, panic abort (cargo forces unwind for bench binaries). That is the shipped
# profile, so the numbers are comparable with the extension. See bottlecap/benches/README.md
# for noise control.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$here/bottlecap"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$crate/target}"
# cargo and critcmp run from the crate, so a relative path must not resolve twice.
[[ "$CARGO_TARGET_DIR" == /* ]] || CARGO_TARGET_DIR="$PWD/$CARGO_TARGET_DIR"

critcmp_bin="${CRITCMP:-}"
if [[ -z "$critcmp_bin" ]] && command -v critcmp >/dev/null 2>&1; then
  critcmp_bin="critcmp"
fi

usage() {
  sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 1
}

need_critcmp() {
  if [[ -z "$critcmp_bin" ]]; then
    echo "critcmp not found. Install it with: cargo install critcmp" >&2
    echo "Or compare with criterion only: cargo bench -- --baseline <old>" >&2
    exit 1
  fi
}

# HEAD, plus a hash of the uncommitted diff when the tree has changes, so two baselines saved
# on the same commit before and after an edit are not reported as the same code.
git_note() {
  local rev
  rev="$(git -C "$here" rev-parse --short HEAD 2>/dev/null)" || { echo unknown; return; }
  if [[ -n "$(git -C "$here" status --porcelain --untracked-files=no)" ]]; then
    rev="$rev-dirty (diff $(git -C "$here" diff HEAD | git hash-object --stdin | cut -c1-12))"
  fi
  echo "$rev"
}

machine_note() {
  local os cpu load
  os="$(uname -sm)"
  if [[ "$(uname -s)" == "Darwin" ]]; then
    cpu="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
  else
    cpu="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ //' || echo unknown)"
  fi
  load="$(uptime | sed 's/.*load average[s]*: //')"
  echo "machine: $os, $cpu; load average: $load; profile: $([[ "${BENCH_FAST:-0}" == 1 ]] && echo 'bench FAST (no LTO, NOT production)' || echo 'bench = release (opt-level z, lto, codegen-units 1)')"
}

cmd="${1:-}"
[[ -n "$cmd" ]] || usage
shift

case "$cmd" in
  save)
    name="${1:-}"
    [[ -n "$name" ]] || usage
    shift
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
    if [[ "${BENCH_FAST:-0}" == 1 ]]; then
      export CARGO_PROFILE_BENCH_LTO=false
      export CARGO_PROFILE_BENCH_CODEGEN_UNITS=16
    fi
    note="$(machine_note)"
    echo "$note"
    # `${arr[@]+"${arr[@]}"}` expands to nothing when the array is empty; a bare "${arr[@]}"
    # is an unbound variable under `set -u` on bash 3.2 (/bin/bash on macOS).
    (cd "$crate" && cargo bench ${benches[@]+"${benches[@]}"} -- --save-baseline "$name" ${extra[@]+"${extra[@]}"})
    mkdir -p "$CARGO_TARGET_DIR/criterion"
    printf '%s\nsaved: %s\ngit: %s\n' "$note" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      "$(git_note)" \
      > "$CARGO_TARGET_DIR/criterion/baseline-$name.txt"
    echo "saved baseline '$name' under $CARGO_TARGET_DIR/criterion (notes in baseline-$name.txt)"
    ;;
  compare)
    old="${1:-}"
    new="${2:-}"
    [[ -n "$old" && -n "$new" ]] || usage
    shift 2
    need_critcmp
    for n in "$old" "$new"; do
      [[ -f "$CARGO_TARGET_DIR/criterion/baseline-$n.txt" ]] && { echo "== $n"; cat "$CARGO_TARGET_DIR/criterion/baseline-$n.txt"; }
    done
    (cd "$crate" && "$critcmp_bin" "$old" "$new" "$@")
    ;;
  export)
    name="${1:-}"
    out="${2:-}"
    [[ -n "$name" && -n "$out" ]] || usage
    need_critcmp
    (cd "$crate" && "$critcmp_bin" --export "$name") > "$out"
    echo "exported '$name' to $out"
    ;;
  list)
    need_critcmp
    (cd "$crate" && "$critcmp_bin" --baselines)
    ;;
  *)
    usage
    ;;
esac
