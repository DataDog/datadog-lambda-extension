#!/bin/sh
# Exported as RUSTC_WORKSPACE_WRAPPER by setup-build-cache.sh. Cargo mixes this
# file's path into the artifact hash of workspace members, so each worktree gets
# its own bottlecap artifacts in the shared target dir. Runs rustc unchanged.
case "${1:-}" in
  -*) exec "${RUSTC:-rustc}" "$@" ;;  # sccache probes the wrapper itself with `-vV`
esac
exec "$@"
