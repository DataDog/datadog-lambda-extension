#!/usr/bin/env bash
# Runs the bottlecap criterion suite with the production profile, saves the results as a named
# baseline, and compares it with another baseline.
#
# Usage:
#   scripts/bench-compare.sh save <name> [bench...] [-- <criterion args>]
#       Run the suite (or only the named benches: logs, traces, invocation, enhanced_metrics,
#       config, init) and save the baseline <name>. Any older results saved as <name> are
#       removed first, so a baseline holds one run.
#   scripts/bench-compare.sh compare <old> <new> [critcmp args]
#       Print a table of <old> vs <new>. Extra args go to critcmp (for example `-t 5` hides
#       differences of 5% or less, `-f 'logs/.*'` filters by regex).
#   scripts/bench-compare.sh export <name> <file.json>
#       Export a baseline to one JSON file, so it survives `cargo clean` and can be compared
#       from another machine with `critcmp old.json new.json`. Its note goes next to it, in
#       <file>.txt.
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
# for noise control, and scripts/bench-note.sh for the note `save` writes.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/bench-note.sh
. "$here/scripts/bench-note.sh"
crate="$here/bottlecap"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$crate/target}"
# cargo and critcmp run from the crate, so a relative path must not resolve twice.
[[ "$CARGO_TARGET_DIR" == /* ]] || CARGO_TARGET_DIR="$PWD/$CARGO_TARGET_DIR"
results="$CARGO_TARGET_DIR/criterion"

critcmp_bin="${CRITCMP:-}"
if [[ -z "$critcmp_bin" ]] && command -v critcmp >/dev/null 2>&1; then
  critcmp_bin="critcmp"
fi

usage() {
  sed -n '2,31p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 1
}

need_critcmp() {
  if [[ -z "$critcmp_bin" ]]; then
    echo "critcmp not found. Install it with: cargo install critcmp" >&2
    echo "Or compare with criterion only: cargo bench -- --baseline <old>" >&2
    exit 1
  fi
}

cmd="${1:-}"
[[ -n "$cmd" ]] || usage
shift

case "$cmd" in
  save)
    name="${1:-}"
    [[ -n "$name" ]] || usage
    shift
    bench_check_name "$name"
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
    note="$(bench_machine_note)"
    # Before the run: the build compiles this tree, and an edit during a long run must not
    # change the recorded revision.
    rev="$(bench_git_rev "$here")"
    echo "$note"
    # `${arr[@]+"${arr[@]}"}` expands to nothing when the array is empty; a bare "${arr[@]}"
    # is an unbound variable under `set -u` on bash 3.2 (/bin/bash on macOS).
    cargo_args=(${benches[@]+"${benches[@]}"} -- --save-baseline "$name" ${extra[@]+"${extra[@]}"})
    bench_clear_baseline "$results" "$name"
    (cd "$crate" && cargo bench "${cargo_args[@]}")
    bench_write_note "$results" "$name" "$note" "$rev" "${cargo_args[*]}"
    bench_check_after_run "$here" "$rev" bench_mark_changed "$(bench_note_file "$results" "$name")"
    echo "saved baseline '$name' under $results (notes in baseline-$name.txt)"
    ;;
  compare)
    old="${1:-}"
    new="${2:-}"
    [[ -n "$old" && -n "$new" ]] || usage
    shift 2
    need_critcmp
    for n in "$old" "$new"; do
      note="$(bench_note_file "$results" "$n")"
      [[ -f "$note" ]] && { echo "== $n"; cat "$note"; }
    done
    (cd "$crate" && "$critcmp_bin" "$old" "$new" "$@")
    ;;
  export)
    name="${1:-}"
    out="${2:-}"
    [[ -n "$name" && -n "$out" ]] || usage
    need_critcmp
    (cd "$crate" && "$critcmp_bin" --export "$name") > "$out"
    # The note travels with the data, so an exported baseline keeps its context.
    note="$(bench_note_file "$results" "$name")"
    if [[ -f "$note" ]]; then
      cp "$note" "${out%.json}.txt"
      echo "exported '$name' to $out, its note to ${out%.json}.txt"
    else
      echo "exported '$name' to $out; it has no note ($note)" >&2
    fi
    ;;
  list)
    need_critcmp
    (cd "$crate" && "$critcmp_bin" --baselines)
    ;;
  *)
    usage
    ;;
esac
