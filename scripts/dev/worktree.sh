#!/usr/bin/env bash
# Create a git worktree that shares the compile cache with every other worktree.
#
#   scripts/dev/worktree.sh <branch> [base]      # create (base default: origin/main, else main)
#   scripts/dev/worktree.sh --list               # list worktrees
#   scripts/dev/worktree.sh --remove <branch>    # remove the worktree (keeps the branch)
#
# Environment (optional):
#   WORKTREE_ROOT   where worktrees go (default: <main checkout>/.worktrees)
#
# The worktree itself holds no build output. All worktrees build into the one
# shared CARGO_TARGET_DIR that scripts/dev/setup-build-cache.sh exports, so a
# new worktree compiles only the bottlecap crate, not its 360 dependencies.
# See scripts/dev/README.md for the measurements.
set -euo pipefail

log() { printf '%s\n' "worktree: $*" >&2; }
die() { log "error: $*"; exit 1; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# --path-format=absolute (git >= 2.31): in the main checkout the plain form is the relative ".git".
common_dir="$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)"
main_root="$(dirname "$common_dir")"
WORKTREE_ROOT="${WORKTREE_ROOT:-$main_root/.worktrees}"
case "$WORKTREE_ROOT" in /*) ;; *) WORKTREE_ROOT="$PWD/$WORKTREE_ROOT" ;; esac

usage() { sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; }
slug() { printf '%s' "$1" | tr '/ ' '--'; }
g() { git -C "$main_root" "$@"; }

case "${1:-}" in
  -h|--help|"") usage; exit 0 ;;
  --list) exec git -C "$main_root" worktree list ;;
  --remove)
    [ $# -eq 2 ] || die "--remove needs a branch name"
    path="$WORKTREE_ROOT/$(slug "$2")"
    [ -d "$path" ] || die "no worktree at $path"
    # slug() maps a/b and a-b to the same path; remove only the requested branch.
    [ "$(git -C "$path" symbolic-ref --quiet --short HEAD || true)" = "$2" ] || die "$path does not have branch $2 checked out"
    g worktree remove "$path"
    log "removed $path (branch $2 still exists; delete it with: git branch -D $2)"
    exit 0 ;;
esac

branch="$1"
base="${2:-}"
path="$WORKTREE_ROOT/$(slug "$branch")"
[ -e "$path" ] && die "$path already exists (use --remove first, or pick another branch name)"
g worktree prune  # forget worktrees whose directory was deleted by hand

if g show-ref --verify --quiet "refs/heads/$branch"; then
  [ -z "$base" ] || log "branch $branch exists; ignoring base $base"
  mkdir -p "$WORKTREE_ROOT"
  g worktree add "$path" "$branch"
elif g show-ref --verify --quiet "refs/remotes/origin/$branch"; then
  [ -z "$base" ] || log "branch origin/$branch exists; ignoring base $base"
  mkdir -p "$WORKTREE_ROOT"
  if ! g worktree add --track -b "$branch" "$path" "origin/$branch" 2>/dev/null; then
    # Single-branch clones cannot track refs outside their fetch refspec.
    log "could not set up tracking of origin/$branch; creating the branch without upstream"
    g worktree add -b "$branch" "$path" "origin/$branch"
  fi
else
  if [ -z "$base" ]; then
    if g rev-parse --verify --quiet origin/main >/dev/null; then base=origin/main; else base=main; fi
    log "base not given; using $base ($(g rev-parse --short "$base")). Run 'git fetch origin main' first if that is stale."
  fi
  g rev-parse --verify --quiet "$base^{commit}" >/dev/null || die "base '$base' is not a commit"
  mkdir -p "$WORKTREE_ROOT"
  g worktree add -b "$branch" "$path" "$base"
fi

qpath="$(printf '%q' "$path/bottlecap")"
cat >&2 <<EOF
worktree: created $path

Next (put the eval and the cargo command in the same shell call):
  cd $qpath
  eval "\$(../scripts/dev/setup-build-cache.sh)"   # shared sccache + shared CARGO_TARGET_DIR
  cargo build                                       # only the bottlecap crate compiles if another worktree built already
  cargo nextest run

Notes:
  - Builds in all worktrees queue on the shared target dir lock. That is expected.
  - target/debug/bottlecap is shared. Tests that spawn the bottlecap binary need
    'eval "\$(../scripts/dev/setup-build-cache.sh --tests)"' (per-worktree target dir).
  - Remove with: scripts/dev/worktree.sh --remove $branch
EOF
printf '%s\n' "$path"
