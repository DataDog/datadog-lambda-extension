# shellcheck shell=bash
# The baseline note of bench-compare.sh and bench-docker-arm64.sh, in one place so the two never
# record different things. Both source this file; the Docker wrapper also sources it inside the
# container, so it runs on /bin/bash 3.2 (macOS) and on Linux.
#
# A baseline <name> under a criterion directory has its note in baseline-<name>.txt: the
# machine, the load average and the profile before the run, the time it was saved, the git
# revision before the run and the cargo arguments, plus a `git at the end:` line when a tracked
# file changed during the run. So a number is never read without its context.

bench_name_re='^[A-Za-z0-9][A-Za-z0-9._-]*$'

# The name is a directory under every bench ID, and bench_clear_baseline deletes it.
bench_check_name() {
  [[ "$1" =~ $bench_name_re ]] || { echo "baseline name must match $bench_name_re: $1" >&2; exit 1; }
}

# bench_note_file <criterion dir> <name>
bench_note_file() {
  echo "$1/baseline-$2.txt"
}

# Criterion keeps a baseline per bench ID and leaves the IDs a run skips (other bench targets,
# a filter) as they were. Remove every result saved as <name>, and its note, so a baseline and
# its note describe one run.
bench_clear_baseline() {
  local dir="$1" name="$2"
  [[ -d "$dir" ]] || return 0
  find "$dir" -type f -path "*/$name/benchmark.json" | sed 's|/benchmark.json$||' |
    while IFS= read -r d; do rm -rf "$d"; done
  rm -f "$(bench_note_file "$dir" "$name")"
}

# HEAD of <repo>, plus a hash of the uncommitted diff when the tree has changes, so two
# baselines saved on the same commit before and after an edit are not reported as the same code.
bench_git_rev() {
  local repo="$1" rev
  rev="$(git -C "$repo" rev-parse --short HEAD 2>/dev/null)" || { echo unknown; return; }
  if [[ -n "$(git -C "$repo" status --porcelain --untracked-files=no)" ]]; then
    rev="$rev-dirty (diff $(git -C "$repo" diff HEAD | git hash-object --stdin | cut -c1-12))"
  fi
  echo "$rev"
}

# The bench profile, with every override from the environment (BENCH_FAST sets two): the bench
# profile inherits the release one, and RUSTFLAGS reach every crate. So a baseline saved with,
# say, CARGO_PROFILE_RELEASE_LTO=false or RUSTFLAGS=-Ctarget-cpu=native is not recorded as
# production.
bench_profile_note() {
  local overrides
  overrides="$(env | grep -E '^(CARGO_PROFILE_(BENCH|RELEASE)_[A-Z0-9_]*|([A-Z0-9_]*_)?RUSTFLAGS)=.' | sort | tr '\n' ' ' || true)"
  if [[ -n "$overrides" ]]; then
    echo "bench with ${overrides}(NOT production)"
  else
    echo 'bench = release (opt-level z, lto, codegen-units 1)'
  fi
}

# The machine, the load average and the profile, where the build runs. In the container,
# BENCH_CONTAINER holds the image and the container limits.
bench_machine_note() {
  local cpu cpus load
  if [[ "$(uname -s)" == "Darwin" ]]; then
    cpu="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)"
    cpus="$(sysctl -n hw.ncpu)"
    load="$(uptime | sed 's/.*load average[s]*: //')"
  else
    cpu="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ //' || true)"
    cpus="$(nproc)"
    load="$(cut -d' ' -f1-3 /proc/loadavg)"
  fi
  echo "machine: $(uname -sm), $cpus cpus visible${cpu:+, $cpu}${BENCH_CONTAINER:+; container: $BENCH_CONTAINER}; load average: $load; profile: $(bench_profile_note)"
}

# bench_write_note <criterion dir> <name> <machine note> <git rev> <cargo args>, with the
# machine note and the revision from before the run.
bench_write_note() {
  local dir="$1" name="$2" machine="$3" rev="$4" args="$5"
  mkdir -p "$dir"
  printf '%s\nsaved: %s\ngit: %s\nargs: %s\n' "$machine" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$rev" "$args" > "$(bench_note_file "$dir" "$name")"
}

# bench_mark_changed <note file> <git rev at the end>
bench_mark_changed() {
  echo "git at the end: $2 (changed during the run)" >> "$1"
}

# bench_check_after_run <repo> <git rev before the run> <command...>: cargo builds what is on
# disk, so an edit to a tracked file during the run gives results that match no one revision.
# When the revision changed, warn and run <command...> with the new revision appended; it
# marks the note (bench_mark_changed, or a container that runs it).
bench_check_after_run() {
  local repo="$1" rev="$2" now
  shift 2
  now="$(bench_git_rev "$repo")"
  [[ "$now" != "$rev" ]] || return 0
  echo "the tracked files changed during the run (git: $rev, then $now); the note records it" >&2
  "$@" "$now"
}
