# Developer scripts

Opt-in helpers to build and test bottlecap fast, for humans and coding agents.
None of them edits shell rc files, `~/.cargo/config.toml` or Docker settings.

| Script | Purpose |
|--------|---------|
| `setup-build-cache.sh` | Install or detect `sccache`, print the env vars for the shared compile cache |
| `worktree.sh` | Create a git worktree that shares the compile cache |

## setup-build-cache.sh

```sh
cd bottlecap
eval "$(../scripts/dev/setup-build-cache.sh)"
cargo build
```

The script installs `sccache` 0.19.0 (prebuilt release, sha256 checked; falls
back to `cargo install`) into `~/.cache/bottlecap/tools/bin` if it is not on
`PATH`, then prints five `export` lines:

| Variable | Default | Why |
|----------|---------|-----|
| `RUSTC_WRAPPER` | path to `sccache` | cache every `rustc` call |
| `SCCACHE_DIR` | `~/.cache/bottlecap/sccache` | object store, shared by every clone and worktree |
| `SCCACHE_CACHE_SIZE` | `20G` | size limit of the store |
| `CARGO_TARGET_DIR` | `~/.cache/bottlecap/target` | one target dir for every clone and worktree |
| `RUSTC_WORKSPACE_WRAPPER` | this worktree's `rustc-workspace-wrapper.sh` | cargo hashes its path into bottlecap's own artifacts, so each worktree keeps its own copy and only dependencies are shared |

Override any of them except `RUSTC_WORKSPACE_WRAPPER` in the environment before you run the script. An inherited
`CARGO_TARGET_DIR` is kept, with a note. On an error the script prints `false`,
so `eval` fails instead of exporting nothing.

| Option | Effect |
|--------|--------|
| `--tests` | Same exports, but `CARGO_TARGET_DIR=<worktree>/bottlecap/target`. Use it for tests that spawn the `bottlecap` binary (`CARGO_BIN_EXE_bottlecap`, the process-level harness): `target/debug/bottlecap` in the shared dir is overwritten by whichever worktree built last. This dir gets no Rust cache hits (see below), so expect one cold build per worktree. |
| `--no-wrapper` | Shared `CARGO_TARGET_DIR` only, `export RUSTC_WRAPPER=` (empty also overrides a `build.rustc-wrapper` in cargo config). |
| `--check` | Print what would be exported. No install, no directories, no server query. |
| `--stats` / `--stop` | `sccache --show-stats` / `sccache --stop-server`. |

The prebuilt download is verified against sha256 values pinned in the script
for v0.19.0 (four targets). With another `SCCACHE_VERSION` the script uses the
release's own `.sha256` file, which only proves integrity, not origin.

### Why one shared `CARGO_TARGET_DIR`

sccache keys a Rust compile on the full `rustc` command line, and cargo passes
`-L dependency=<CARGO_TARGET_DIR>/debug/deps`. A different target dir path is a
different key.

Measured on a 16-core Apple Silicon Mac with other builds running (load 5 to 17):

| Case | `cargo build` | then `cargo test --no-run` |
|------|---------------|----------------------------|
| Cold, no cache | 57.6 s | +29.9 s |
| Warm sccache, new target dir path | 65.6 s (0 Rust hits, 463 C/asm hits) | +31.8 s |
| Warm sccache, same path, target dir deleted | 57.3 s (827 of 827 hits) | +22.0 s (151 of 151 hits) |
| Shared target dir, second worktree, first build | 15.5 s | +16.7 s |
| No-op rebuild | 0.8 s | |

The shared target dir is what saves time. Cargo reuses the 360 dependency
artifacts through its own fingerprints, so a new worktree compiles only the
`bottlecap` crate. sccache alone does not shorten a cold build on a 16-core
machine, because the critical path is work it cannot cache
(`cargo build --timings`, run under load): the `tikv-jemalloc-sys` build script
(`configure` + `make`, 59 s), the `aws-lc-sys` build script (cmake, 21 s;
pulled in by the `reqwest` build-dependency of `libddwaf-sys`, not linked into
the binary), `libddwaf-sys` (10 s) and `zstd-sys` (9 s) build scripts, and then
the `bottlecap` crate itself (42 s + 11 s for the bin). sccache still pays off
when CPU is scarce (CI, the 4-CPU Docker VM, or 12 agents building at once): with
100 % hits the Rust dependency compiles cost almost no CPU. The `cc` crate also
routes C compiles through sccache when `RUSTC_WRAPPER` ends in `sccache`, which
is where the C/C++ hits come from.

### Lock contention

Cargo takes a whole-build lock per profile dir (`target/debug/.cargo-lock`).
Two worktrees that build at the same time queue; debug and release builds do
not block each other.

| Two worktrees touch `src/lib.rs` and build at once | Wall time |
|---|---|
| Shared target dir | 30.1 s (15.3 s + 29.5 s, the second waits on the lock) |
| One target dir per worktree | 9.0 s (8.5 s and 5.0 s in parallel) |
| One incremental build alone, equal load | shared 6.1 s, own 6.5 s |

Recommendation: share the target dir. The lock costs at most one queued build
(about 15 s), and saves about 60 s and 3 to 5 GB per worktree. Fifteen
per-worktree target dirs held 67 GB; the shared dir with debug
and test artifacts of two worktrees holds 7.5 GB.

Rules for the shared dir:

- Never run `cargo clean` there. It deletes every worktree's artifacts.
- `target/debug/bottlecap` is overwritten by whichever worktree built last.
  Use `cargo run --bin bottlecap`, or read the path from
  `cargo build --message-format=json | jq -r 'select(.executable != null).executable'`.
  Tests that spawn that binary (`CARGO_BIN_EXE_bottlecap`) must run with
  `setup-build-cache.sh --tests`. Unit test and bench binaries in
  `target/debug/deps/` carry a per-worktree hash (from `RUSTC_WORKSPACE_WRAPPER`)
  and do not collide. Run the `eval` in the worktree you build in. sccache
  still caches dependencies; it runs bottlecap's own compiles uncached.
- `cargo clippy` sets its own `RUSTC_WORKSPACE_WRAPPER`, so in the shared dir it
  can replay another worktree's result. Run clippy with `setup-build-cache.sh --tests`.
- `target/criterion/` is shared. Use `cargo bench -- --save-baseline <name>`.
- Builds that wait on the lock print `Blocking waiting for file lock on build directory`.
  Give agent shells a long timeout or run builds in the background.
- The sccache wrapper adds about 2 s to an incremental build (6.3 s vs 4.4 s).
  For a long edit-compile loop in one worktree, `export RUSTC_WRAPPER=` and keep `CARGO_TARGET_DIR`.
- The sccache server reads `SCCACHE_DIR` and `SCCACHE_CACHE_SIZE` once, at start.
  A client with another `SCCACHE_DIR` silently uses the running server. The script warns about that.

## worktree.sh

```sh
scripts/dev/worktree.sh jordan.gonzalez/logs/fix            # from origin/main (or main)
scripts/dev/worktree.sh jordan.gonzalez/logs/fix v89         # from a tag or commit
scripts/dev/worktree.sh --list
scripts/dev/worktree.sh --remove jordan.gonzalez/logs/fix    # keeps the branch
```

Worktrees go to `<main checkout>/.worktrees/<branch with / as ->` (override with
`WORKTREE_ROOT`). The directory is in `.gitignore`. A branch that exists only
on `origin` is checked out with tracking. The script prints the exact build
commands for the new worktree. All worktrees build into the shared
`CARGO_TARGET_DIR`; see the measurements above for why there is no per-worktree
option, and `setup-build-cache.sh --tests` for the one case that needs one.

## CI release binaries per commit

GitLab compiles each flavor in the `bottlecap (<flavor>)` job
(`.gitlab/templates/pipeline.yaml.tpl`) and keeps the stripped binary
`.binaries/bottlecap-<suffix>` for one week. On `main` a second job,
`bottlecap symbols (<flavor>)`, runs for `amd64` and `arm64`
(`keep_symbols: true` in `.gitlab/datasources/flavors.yaml`). It compiles the
same release profile with `CARGO_PROFILE_RELEASE_STRIP=false` and publishes,
for 90 days, under the artifact name `bottlecap-<suffix>-<short sha>`:

- `.binaries/bottlecap-<suffix>` (the shipped stripped binary, copied from the compile job)
- `.binaries/bottlecap-<suffix>-unstripped` (same compile, symbol table kept; never shipped)
- `.binaries/bottlecap-<suffix>.build-info` (commit, ref, pipeline and job URLs)

The job also runs on tags and is manual on other branches. Every rule has
`allow_failure: true` and the retry covers only runner and timeout failures,
so a broken symbols build never turns `main` red or blocks a release. The
shipped layer does not change.

Download the latest `main` build (needs a GitLab token with `read_api`):

```sh
API=https://gitlab.ddbuild.io/api/v4/projects/DataDog%2Fdatadog-lambda-extension
JOB='bottlecap symbols (arm64)'
curl -fsSL -H "PRIVATE-TOKEN: $GITLAB_TOKEN" -o bottlecap-arm64-unstripped \
  "$API/jobs/artifacts/main/raw/.binaries/bottlecap-arm64-unstripped?job=$(printf %s "$JOB" | jq -sRr @uri)"
```

For a given commit, list its pipeline jobs and download the job's artifact zip
(`$API/pipelines?sha=<sha>`, then `$API/pipelines/<id>/jobs`, then
`$API/jobs/<job id>/artifacts`). The job log also prints the raw download URL
of the unstripped binary.
