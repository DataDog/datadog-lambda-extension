#!/usr/bin/env bash
# Opt-in shared compile cache for bottlecap builds (sccache + one shared target dir).
#
# This script never edits shell rc files or ~/.cargo/config.toml. It prints
# `export` lines on stdout. Load them into the current shell with:
#
#   eval "$(scripts/dev/setup-build-cache.sh)"
#
# Other commands:
#   scripts/dev/setup-build-cache.sh --tests       # same, but a per-worktree target dir (tests that spawn the bottlecap binary)
#   scripts/dev/setup-build-cache.sh --no-wrapper  # shared CARGO_TARGET_DIR only, no sccache
#   scripts/dev/setup-build-cache.sh --check       # print what would be exported; no install, no dirs, no server query
#   scripts/dev/setup-build-cache.sh --stats       # sccache hit/miss stats
#   scripts/dev/setup-build-cache.sh --stop        # stop the sccache server
#
# Environment (all optional):
#   BOTTLECAP_CACHE_ROOT  root for cache and tools    (default: $HOME/.cache/bottlecap)
#   SCCACHE_DIR           sccache object store        (default: $BOTTLECAP_CACHE_ROOT/sccache)
#   SCCACHE_CACHE_SIZE    sccache size limit          (default: 20G)
#   CARGO_TARGET_DIR      shared cargo target dir     (default: $BOTTLECAP_CACHE_ROOT/target)
#   SCCACHE_VERSION       version to install          (default: 0.19.0, sha256 pinned below)
#   SCCACHE_INSTALL       prebuilt | cargo | none     (default: prebuilt, then cargo)
#
# Why one shared CARGO_TARGET_DIR: sccache hashes the full rustc command line,
# and cargo passes `-L dependency=<CARGO_TARGET_DIR>/debug/deps`. A different
# target dir path gives 0 % Rust cache hits. See scripts/dev/README.md.
# RUSTC_WORKSPACE_WRAPPER points at this worktree's rustc-workspace-wrapper.sh,
# so bottlecap's own artifacts stay per worktree and only dependencies are shared.
set -euo pipefail

log() { printf '%s\n' "setup-build-cache: $*" >&2; }
die() { log "error: $*"; exit 1; }

TMP=""
# On any failure, `false` on stdout makes `eval "$(...)"` fail instead of exporting nothing.
# shellcheck disable=SC2317,SC2329  # invoked by the trap
cleanup() {
  local rc=$?
  [ -z "$TMP" ] || rm -rf "$TMP"
  [ "$rc" -eq 0 ] || printf 'false\n'
}
trap cleanup EXIT

# Exported paths must not depend on the directory cargo later runs from.
abspath() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$PWD" "$1" ;; esac; }
BOTTLECAP_CACHE_ROOT="$(abspath "${BOTTLECAP_CACHE_ROOT:-$HOME/.cache/bottlecap}")"
DEFAULT_TARGET_DIR="$BOTTLECAP_CACHE_ROOT/target"
SCCACHE_DIR="$(abspath "${SCCACHE_DIR:-$BOTTLECAP_CACHE_ROOT/sccache}")"
SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-20G}"
CARGO_TARGET_DIR="$(abspath "${CARGO_TARGET_DIR:-$DEFAULT_TARGET_DIR}")"
SCCACHE_VERSION="${SCCACHE_VERSION:-0.19.0}"
SCCACHE_INSTALL="${SCCACHE_INSTALL:-prebuilt}"
TOOLS_DIR="$BOTTLECAP_CACHE_ROOT/tools"
WORKSPACE_WRAPPER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/rustc-workspace-wrapper.sh"
PINNED_VERSION="0.19.0"
mode="export"
wrapper=1
target_mode="shared"

case "${1:-}" in
  "") ;;
  --tests)
    repo_root="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null)" || die "--tests must run inside the git checkout"
    CARGO_TARGET_DIR="$repo_root/bottlecap/target"
    target_mode="per-worktree" ;;
  --no-wrapper) wrapper=0 ;;
  --check) mode="check" ;;
  --stats) mode="stats" ;;
  --stop) mode="stop" ;;
  -h|--help) sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) die "unknown option: $1 (see --help)" ;;
esac

if [ "$target_mode" = "shared" ] && [ "$CARGO_TARGET_DIR" != "$DEFAULT_TARGET_DIR" ]; then
  log "note: CARGO_TARGET_DIR was already set to $CARGO_TARGET_DIR; keeping it. Other worktrees share this cache only with the same path."
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Release checksums of sccache v0.19.0 (https://github.com/mozilla/sccache/releases/tag/v0.19.0).
pinned_sha256() {
  case "$1" in
    aarch64-apple-darwin) echo a35a0b7d622618774bc1bb8102d72562dfb21e619f88b153b370d7f89bc286af ;;
    x86_64-apple-darwin) echo 2db7344bb96de350df79cd9f0124dd4e9baa47b3de2fac1e63e1346b66b20684 ;;
    x86_64-unknown-linux-musl) echo aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd ;;
    aarch64-unknown-linux-musl) echo 3e47fc4a71b1ddd1b4294a0de9d34498922fdf41e3cd0bda5df3954e3d6deb9d ;;
    *) return 1 ;;
  esac
}

prebuilt_triple() {
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) echo aarch64-apple-darwin ;;
    Darwin-x86_64) echo x86_64-apple-darwin ;;
    Linux-x86_64) echo x86_64-unknown-linux-musl ;;
    Linux-aarch64|Linux-arm64) echo aarch64-unknown-linux-musl ;;
    *) return 1 ;;
  esac
}

install_prebuilt() {
  local triple base url want have
  if ! triple="$(prebuilt_triple)"; then log "no prebuilt sccache for $(uname -s)-$(uname -m)"; return 1; fi
  base="sccache-v${SCCACHE_VERSION}-${triple}"
  url="https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${base}.tar.gz"
  TMP="$(mktemp -d)" || return 1
  log "downloading $url"
  if ! curl -fsSL --retry 3 -o "$TMP/$base.tar.gz" "$url"; then log "download failed"; return 1; fi
  if [ "$SCCACHE_VERSION" = "$PINNED_VERSION" ]; then
    want="$(pinned_sha256 "$triple")"
  else
    log "SCCACHE_VERSION=$SCCACHE_VERSION is not the pinned $PINNED_VERSION; using the release's own .sha256 (integrity only)"
    if ! curl -fsSL --retry 3 -o "$TMP/$base.tar.gz.sha256" "$url.sha256"; then log "checksum download failed"; return 1; fi
    want="$(cut -d' ' -f1 < "$TMP/$base.tar.gz.sha256")"
  fi
  have="$(sha256_file "$TMP/$base.tar.gz")"
  [ "$want" = "$have" ] || die "sha256 mismatch for $base.tar.gz: expected $want, got $have"
  # errexit is off here (called from `if !`), so return on each failure to reach the cargo fallback.
  tar -xzf "$TMP/$base.tar.gz" -C "$TMP" || return 1
  mkdir -p "$TOOLS_DIR/bin" || return 1
  install -m 0755 "$TMP/$base/sccache" "$TOOLS_DIR/bin/sccache" || return 1
  log "installed sccache $SCCACHE_VERSION to $TOOLS_DIR/bin/sccache"
}

install_cargo() {
  log "running: cargo install sccache --locked --version $SCCACHE_VERSION --root $TOOLS_DIR (this takes a few minutes)"
  cargo install sccache --locked --version "$SCCACHE_VERSION" --root "$TOOLS_DIR" >&2
}

find_sccache() {
  if [ -n "${SCCACHE_BIN:-}" ] && [ -x "$SCCACHE_BIN" ]; then echo "$SCCACHE_BIN"; return 0; fi
  if [ -x "$TOOLS_DIR/bin/sccache" ]; then echo "$TOOLS_DIR/bin/sccache"; return 0; fi
  if command -v sccache >/dev/null 2>&1; then command -v sccache; return 0; fi
  return 1
}

print_exports() {
  if [ "$wrapper" = 1 ]; then
    printf 'export RUSTC_WRAPPER=%q\n' "$SCCACHE_BIN_PATH"
    printf 'export SCCACHE_DIR=%q\n' "$SCCACHE_DIR"
    printf 'export SCCACHE_CACHE_SIZE=%q\n' "$SCCACHE_CACHE_SIZE"
    if [ "$SCCACHE_BIN_PATH" = "$TOOLS_DIR/bin/sccache" ]; then
      # shellcheck disable=SC2016  # $PATH must expand in the caller's shell
      case ":$PATH:" in
        *":$TOOLS_DIR/bin:"*) ;;
        *) printf 'export PATH=%q:"$PATH"\n' "$TOOLS_DIR/bin" ;;
      esac
    fi
  else
    # Empty, not unset: an empty RUSTC_WRAPPER also overrides build.rustc-wrapper in cargo config.
    printf 'export RUSTC_WRAPPER=\n'
  fi
  printf 'export RUSTC_WORKSPACE_WRAPPER=%q\n' "$WORKSPACE_WRAPPER"
  printf 'export CARGO_TARGET_DIR=%q\n' "$CARGO_TARGET_DIR"
}

SCCACHE_BIN_PATH="$(find_sccache || true)"

case "$mode" in
  stats) [ -n "$SCCACHE_BIN_PATH" ] || die "sccache not found; run without options to install it"; export SCCACHE_DIR SCCACHE_CACHE_SIZE; exec "$SCCACHE_BIN_PATH" --show-stats ;;
  stop) [ -n "$SCCACHE_BIN_PATH" ] || die "sccache not found"; exec "$SCCACHE_BIN_PATH" --stop-server ;;
  check)
    if [ -z "$SCCACHE_BIN_PATH" ]; then
      SCCACHE_BIN_PATH="$TOOLS_DIR/bin/sccache"
      log "sccache is not installed; a plain run installs $SCCACHE_VERSION to $SCCACHE_BIN_PATH"
    fi
    log "would export (no files or servers touched):"
    print_exports
    exit 0 ;;
esac

if [ "$wrapper" = 1 ] && [ -z "$SCCACHE_BIN_PATH" ]; then
  case "$SCCACHE_INSTALL" in
    prebuilt)
      if ! install_prebuilt; then
        log "prebuilt install failed; falling back to cargo install"
        install_cargo
      fi ;;
    cargo) install_cargo ;;
    none) die "sccache not found and SCCACHE_INSTALL=none" ;;
    *) die "SCCACHE_INSTALL must be prebuilt, cargo or none" ;;
  esac
  SCCACHE_BIN_PATH="$(find_sccache)" || die "install succeeded but sccache is still not found"
fi

mkdir -p "$CARGO_TARGET_DIR"
if [ "$wrapper" = 1 ]; then
  mkdir -p "$SCCACHE_DIR"
  # The sccache server reads SCCACHE_DIR and SCCACHE_CACHE_SIZE once, when it
  # starts. A client with a different SCCACHE_DIR silently uses the running
  # server. Detect that and tell the user how to fix it.
  realdir() { (cd "$1" 2>/dev/null && pwd -P) || printf '%s' "$1"; }
  running_dir="$(SCCACHE_DIR="$SCCACHE_DIR" SCCACHE_CACHE_SIZE="$SCCACHE_CACHE_SIZE" "$SCCACHE_BIN_PATH" --show-stats 2>/dev/null | sed -n 's/^Cache location *Local disk: "\(.*\)"$/\1/p' || true)"
  if [ -n "$running_dir" ] && [ "$(realdir "$running_dir")" != "$(realdir "$SCCACHE_DIR")" ]; then
    log "WARNING: an sccache server is already running with SCCACHE_DIR=$running_dir"
    log "         this shell asked for $SCCACHE_DIR. Run '$SCCACHE_BIN_PATH --stop-server' first,"
    log "         or export SCCACHE_DIR=$running_dir to join the running server."
  fi
  log "sccache:          $SCCACHE_BIN_PATH ($("$SCCACHE_BIN_PATH" --version 2>/dev/null | tr -d '\n'))"
  log "SCCACHE_DIR:      $SCCACHE_DIR (limit $SCCACHE_CACHE_SIZE)"
fi
if [ "$target_mode" = "shared" ]; then
  log "CARGO_TARGET_DIR: $CARGO_TARGET_DIR (shared by all worktrees; cargo serializes builds on its lock)"
else
  log "CARGO_TARGET_DIR: $CARGO_TARGET_DIR (per-worktree; sccache gives no Rust hits here, see README)"
fi
if [ -t 1 ]; then
  log "stdout is a terminal. To apply these in your shell run:  eval \"\$($0${1:+ $1})\""
fi
print_exports
exit 0
