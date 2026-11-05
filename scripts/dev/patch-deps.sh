#!/usr/bin/env bash
# Point bottlecap at a local checkout of a dependency repo, and undo it again.
#
#   scripts/dev/patch-deps.sh add <checkout>          # e.g. ../serverless-components or ~/src/libdatadog
#   scripts/dev/patch-deps.sh remove <checkout|name>  # drop one checkout
#   scripts/dev/patch-deps.sh restore [--force]       # drop all, restore Cargo.lock
#   scripts/dev/patch-deps.sh status
#
# `add` reads `cargo metadata` of the checkout and of bottlecap, takes every
# crate that appears in both, groups them by source (crates.io or git URL) and
# writes one managed `[patch]` region at the end of bottlecap/Cargo.toml.
# It backs up Cargo.lock once, because cargo rewrites it for patched crates.
# Works for serverless-components, libdatadog, saluki, dd-trace-rs and libddwaf-rust.
#
# When a local version differs from the locked one, `add` runs `cargo update` on
# that crate so cargo selects the patch. Cargo ignores a patch whose version does
# not match bottlecap's requirements; the script then prints "NOT USED".
# `add`, `remove` and `restore` refuse to run when Cargo.lock changed since the
# last patch-deps run (for example after you added a dependency); `restore --force`
# restores the backup anyway. Never commit the managed region or the patched lock.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -I - "$here/../../bottlecap" "$@" <<'PY'
import hashlib, json, os, re, shutil, subprocess, sys

BEGIN = "# >>> patch-deps: managed by scripts/dev/patch-deps.sh. Do not commit. >>>"
END = "# <<< patch-deps <<<"
REGION = re.compile(re.escape(BEGIN) + r".*?" + re.escape(END) + r"\n?", re.S)

bottlecap = os.path.abspath(sys.argv[1])
manifest = os.path.join(bottlecap, "Cargo.toml")
lock = os.path.join(bottlecap, "Cargo.lock")
state_dir = os.path.join(bottlecap, ".patch-deps")
state_file = os.path.join(state_dir, "state.json")
lock_backup = os.path.join(state_dir, "Cargo.lock.orig")
lock_hash_file = os.path.join(state_dir, "Cargo.lock.sha256")
args = sys.argv[2:]
force = "--force" in args
args = [a for a in args if a != "--force"]


def die(msg, code=1):
    print(f"patch-deps: error: {msg}", file=sys.stderr)
    sys.exit(code)


def metadata(path, no_deps):
    cmd = ["cargo", "metadata", "--format-version", "1", "--manifest-path", path]
    if no_deps:
        cmd.append("--no-deps")
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        die(f"cargo metadata failed for {path}:\n{r.stderr}")
    return json.loads(r.stdout), r.stderr


def patch_key(source):
    # registry+https://github.com/rust-lang/crates.io-index  -> crates-io
    # sparse+https://index.crates.io/                        -> crates-io
    # git+https://github.com/DataDog/saluki/?rev=abc#abc    -> https://github.com/DataDog/saluki/
    if source.startswith(("registry+", "sparse+")):
        return "crates-io"
    if source.startswith("git+"):
        return source[4:].split("#", 1)[0].split("?", 1)[0]
    return None


def sha256_of(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def load_state():
    if os.path.exists(state_file):
        with open(state_file) as f:
            return json.load(f)
    return {}


def save_state(state):
    os.makedirs(state_dir, exist_ok=True)
    with open(state_file, "w") as f:
        json.dump(state, f, indent=2, sort_keys=True)


def read_manifest():
    with open(manifest) as f:
        return f.read()


def write_region(state):
    """Rewrite the managed region. Dies before touching any file when the manifest has a foreign [patch]."""
    text = REGION.sub("", read_manifest()).rstrip("\n") + "\n"
    stray = [l for l in text.splitlines() if l.startswith("[patch")]
    if stray:
        die(f"Cargo.toml already has a [patch] table outside the managed region: {stray}")
    if state:
        tables = {}
        for name, entries in sorted(state.items()):
            for crate, info in sorted(entries["crates"].items()):
                for key in info["keys"]:
                    tables.setdefault(key, []).append((crate, info["path"], name))
        lines = ["", BEGIN]
        for key, crates in sorted(tables.items()):
            lines.append(f"[patch.{key}]" if key == "crates-io" else f'[patch."{key}"]')
            for crate, path, name in crates:
                lines.append(f"{crate} = {{ path = {json.dumps(path, ensure_ascii=False)} }}  # {name}")
        lines.append(END)
        text += "\n".join(lines) + "\n"
    with open(manifest, "w") as f:
        f.write(text)


def backup_lock():
    """Take a fresh backup. Called only when no patch was active, so the lock is the pristine one."""
    if os.path.exists(lock):
        os.makedirs(state_dir, exist_ok=True)
        shutil.copy2(lock, lock_backup)


def record_lock_hash():
    if os.path.exists(lock):
        os.makedirs(state_dir, exist_ok=True)
        with open(lock_hash_file, "w") as f:
            f.write(sha256_of(lock))


def lock_unchanged():
    if not os.path.exists(lock_hash_file):
        return True
    with open(lock_hash_file) as f:
        recorded = f.read().strip()
    return os.path.exists(lock) and recorded == sha256_of(lock)


def check_lock_unchanged():
    if not (force or lock_unchanged()):
        die("Cargo.lock changed or was deleted since the last patch-deps run (did you add or bump a dependency?). "
            "Inspect it with `git diff bottlecap/Cargo.lock`, then run `restore --force` to restore the backup anyway.", code=4)


def restore_lock():
    if os.path.exists(lock_backup):
        shutil.move(lock_backup, lock)
        print("patch-deps: restored Cargo.lock", file=sys.stderr)


def resolve_and_report(state, record=True):
    """Re-run cargo metadata on bottlecap so Cargo.lock updates, then show what is in effect."""
    meta, stderr = metadata(manifest, no_deps=False)
    unused = [l.strip() for l in stderr.splitlines() if "was not used in the crate graph" in l]
    ok = not unused
    for name, entries in sorted(state.items()):
        print(f"{name} ({entries['checkout']})")
        for crate, info in sorted(entries["crates"].items()):
            active = any(p["name"] == crate and p["source"] is None and os.path.dirname(p["manifest_path"]) == info["path"]
                         for p in meta["packages"])
            ok = ok and active
            status = "active" if active else "NOT USED (version not compatible with the locked one?)"
            print(f"  {crate:28s} locked {', '.join(info['locked']):>8s}  local {info['local']:>8s}  {status}")
    for l in unused:
        print(f"  cargo: {l}", file=sys.stderr)
    if record:
        record_lock_hash()
    return ok


def cmd_add(checkout):
    checkout = os.path.abspath(os.path.expanduser(checkout))
    cm = os.path.join(checkout, "Cargo.toml")
    if not os.path.exists(cm):
        die(f"{cm} not found")
    local, _ = metadata(cm, no_deps=True)
    local_pkgs = {p["name"]: p for p in local["packages"]}
    name = os.path.basename(checkout.rstrip("/"))
    state = load_state()
    old = state.get(name, {}).get("checkout")
    if old and os.path.exists(old) and not os.path.samefile(old, checkout):
        die(f"{name} is already patched from {old}. Run `remove {name}` first.")
    check_lock_unchanged()
    # Point an earlier `add` of this checkout at the new path first, so a moved checkout still resolves.
    patched = {c: dict(info, path=os.path.dirname(local_pkgs[c]["manifest_path"]))
               for c, info in state.get(name, {}).get("crates", {}).items() if c in local_pkgs}
    if name in state:
        write_region({**state, name: {"checkout": checkout, "crates": patched}})
    graph, _ = metadata(manifest, no_deps=False)
    crates, updates = {}, []
    for p in graph["packages"]:
        if p["name"] not in local_pkgs:
            continue
        lp = local_pkgs[p["name"]]
        if not p["source"]:
            # Already patched by an earlier `add` of this checkout: cargo shows it as a path package.
            prev = patched.get(p["name"])
            if not (prev and os.path.dirname(p["manifest_path"]) == prev["path"]):
                continue
            keys, locked = prev["keys"], prev["locked"]
        else:
            key = patch_key(p["source"])
            if key is None:
                continue
            keys, locked = [key], [p["version"]]
            if p["version"] != lp["version"]:
                # Cargo keeps a locked version that differs from the patch, so unlock it. For crates.io,
                # --precise selects the local version, or fails and changes nothing if no requirement allows it.
                updates.append(["-p", p["id"]] + (["--precise", lp["version"]] if key == "crates-io" else []))
        # The same crate can come from several sources or versions: patch each source.
        info = crates.setdefault(p["name"], {"keys": [], "locked": [], "path": os.path.dirname(lp["manifest_path"]),
                                             "local": lp["version"]})
        info["keys"] = sorted(set(info["keys"] + keys))
        info["locked"] = sorted(set(info["locked"] + locked))
    if not crates:
        die(f"no crate of {checkout} is in bottlecap's dependency graph")
    fresh = not state
    new_state = dict(state)
    new_state[name] = {"checkout": checkout, "crates": crates}
    write_region(new_state)
    if fresh:
        backup_lock()
    save_state(new_state)
    for c, info in sorted(crates.items()):
        if all(v.split(".")[0] != info["local"].split(".")[0] for v in info["locked"]):
            print(f"patch-deps: warning: {c} local {info['local']} vs locked {', '.join(info['locked'])}: major version differs, cargo will not use the patch",
                  file=sys.stderr)
    for u in updates:
        subprocess.run(["cargo", "update", "--manifest-path", manifest] + u, capture_output=True)
    ok = resolve_and_report(new_state)
    print(f"patch-deps: {len(crates)} crate(s) from {name} written to {manifest}", file=sys.stderr)
    sys.exit(0 if ok else 3)


def cmd_remove(which):
    state = load_state()
    name = which if which in state else os.path.basename(os.path.abspath(os.path.expanduser(which)).rstrip("/"))
    if name not in state:
        die(f"{which} is not patched (known: {', '.join(state) or 'none'})")
    check_lock_unchanged()
    if len(state) == 1:
        cmd_restore()
        return
    del state[name]
    write_region(state)
    save_state(state)
    ok = resolve_and_report(state)
    print(f"patch-deps: removed {name}", file=sys.stderr)
    sys.exit(0 if ok else 3)


def cmd_restore():
    check_lock_unchanged()
    write_region({})
    restore_lock()
    shutil.rmtree(state_dir, ignore_errors=True)
    print("patch-deps: Cargo.toml has no managed region now", file=sys.stderr)


def cmd_status():
    state = load_state()
    if not state:
        print("patch-deps: nothing patched")
        return
    # Record the lock update that this resolve makes, but never accept an earlier foreign edit.
    sys.exit(0 if resolve_and_report(state, record=lock_unchanged()) else 3)


if not args or args[0] in ("-h", "--help"):
    print("usage: patch-deps.sh add <checkout> | remove <checkout|name> | restore [--force] | status", file=sys.stderr)
    sys.exit(2)
cmd, rest = args[0], args[1:]
if force and cmd != "restore":
    die("--force is only valid with restore", code=2)
if cmd == "add" and len(rest) == 1:
    cmd_add(rest[0])
elif cmd == "remove" and len(rest) == 1:
    cmd_remove(rest[0])
elif cmd == "restore" and not rest:
    cmd_restore()
elif cmd == "status" and not rest:
    cmd_status()
else:
    die("bad arguments. usage: add <checkout> | remove <checkout|name> | restore [--force] | status", code=2)
PY
