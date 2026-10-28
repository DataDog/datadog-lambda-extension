#!/usr/bin/env python3
"""JSON report on a Linux ELF64 binary. No readelf, no cargo bloat, stdlib only.

    binary_report.py <binary> [--diff <other>] [--top N] [--no-crates]

Reports file size, allocated section sizes, PT_LOAD sizes, dynamic relocation
counts (RELATIVE from .rela.dyn and .relr.dyn), NEEDED libraries, BIND_NOW and
PIE flags, and the top crates by .text size from .symtab (unstripped binaries only).
"""
import argparse
import json
import os
import re
import struct
import sys
from collections import defaultdict

SHT_SYMTAB, SHT_STRTAB, SHT_RELA, SHT_DYNAMIC, SHT_NOBITS, SHT_RELR = 2, 3, 4, 6, 8, 19
SHF_ALLOC = 0x2
PT_LOAD, PT_INTERP = 1, 3
DT = {1: "NEEDED", 2: "PLTRELSZ", 5: "STRTAB", 7: "RELA", 8: "RELASZ", 9: "RELAENT", 23: "JMPREL",
      24: "BIND_NOW", 30: "FLAGS", 35: "RELRSZ", 36: "RELR", 0x6FFFFFF9: "RELACOUNT", 0x6FFFFFFB: "FLAGS_1"}
DF = {0x1: "ORIGIN", 0x2: "SYMBOLIC", 0x4: "TEXTREL", 0x8: "BIND_NOW", 0x10: "STATIC_TLS"}
DF_1 = {0x1: "NOW", 0x8: "NODELETE", 0x80: "NODEFLIB", 0x08000000: "PIE"}
MACHINE = {0xB7: "aarch64", 0x3E: "x86_64"}
R_RELATIVE = {0xB7: 1027, 0x3E: 8}
ETYPE = {2: "EXEC", 3: "DYN"}

# libddwaf is the only C++ code linked, so every C++ (`_Z`, not legacy Rust) symbol counts there.
C_HINTS = [
    ("[C libddwaf]", re.compile(r"(^_Z|ddwaf|std::__[12]|__cxx|libinjection|re2::|fmt::v|rapidjson|operator new|__cxa|__gxx|__dynamic_cast|typeinfo|vtable for|guard variable|__lcxx|_GLOBAL__sub_I|utf8proc|ac_|lua_|pcre)")),
    ("[C zstd]", re.compile(r"^(ZSTD|HUF_|FSE_|ZDICT|COVER|HIST_|POOL_|ERR_getErrorString|ZBUFF)")),
    ("[C jemalloc]", re.compile(r"^(_rjem|je_|malloc_|jemalloc)|rjem")),
    ("[C/asm ring]", re.compile(r"^(ring_core|GFp|ring_|aes_|gcm_|sha256|sha512|ChaCha20|chacha|x25519|p256|p384|ecp_|bn_|LIMBS|OPENSSL|CRYPTO|poly1305|fiat_)")),
]
# Legacy Rust symbols end in a 17h<16 hex>E hash segment, optionally followed by a .llvm.N style suffix; C++ ones do not.
LEGACY_RUST = re.compile(r"17h[0-9a-f]{16}E(?:\.|$)")
V0_CRATE = re.compile(r"^_R(?:[A-Za-z]|s[0-9A-Za-z]*_)*?C(?:s[0-9A-Za-z]*_)?(\d+)(.*)$")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def crate_of(sym):
    if sym.startswith("_ZN") and LEGACY_RUST.search(sym):
        m = re.match(r"^_ZN(\d+)", sym)
        if not m:
            return "[unknown]"
        n = int(m.group(1))
        first = sym[len(m.group(0)):len(m.group(0)) + n]
        if first.startswith("_$LT$") or first.startswith("$LT$"):
            for esc, ch in (("$LT$", "<"), ("$GT$", ">"), ("$RF$", "&"), ("$BP$", "*"), ("$u20$", " "), ("..", "::")):
                first = first.replace(esc, ch)
            first = re.sub(r"^_?<[&*]?(?:mut |dyn |const )*", "", first)
        m2 = IDENT.match(first)
        return m2.group(0) if m2 else "[unknown]"
    if sym.startswith("_R"):
        m = V0_CRATE.match(sym)
        if m:
            n, rest = int(m.group(1)), m.group(2)
            if rest.startswith("_"):
                rest = rest[1:]
            return rest[:n] or "[unknown]"
        return "[unknown]"
    for name, rx in C_HINTS:
        if rx.search(sym):
            return name
    if sym.startswith(("__rust", "rust_", "__rdl_", "__rg_")):
        return "std"
    return "[other C/unknown]"


class Elf:
    def __init__(self, path):
        self.path = path
        with open(path, "rb") as f:
            self.data = f.read()
        d = self.data
        if d[:4] != b"\x7fELF" or d[4] != 2 or d[5] != 1:
            raise SystemExit(f"{path}: not a little-endian ELF64 file")
        (_, self.e_type, self.e_machine, _, self.e_entry, self.e_phoff, self.e_shoff, _, _,
         self.e_phentsize, self.e_phnum, self.e_shentsize, self.e_shnum, self.e_shstrndx) = struct.unpack_from("<16sHHIQQQIHHHHHH", d, 0)
        self.sections = []
        for i in range(self.e_shnum):
            off = self.e_shoff + i * self.e_shentsize
            s = dict(zip(("name", "type", "flags", "addr", "offset", "size", "link", "info", "addralign", "entsize"),
                         struct.unpack_from("<IIQQQQIIQQ", d, off)))
            self.sections.append(s)
        shstr = self.sections[self.e_shstrndx]
        for s in self.sections:
            s["name"] = self.cstr(shstr["offset"] + s["name"])
        self.segments = []
        for i in range(self.e_phnum):
            off = self.e_phoff + i * self.e_phentsize
            self.segments.append(dict(zip(("type", "flags", "offset", "vaddr", "paddr", "filesz", "memsz", "align"),
                                          struct.unpack_from("<IIQQQQQQ", d, off))))

    def cstr(self, off):
        end = self.data.index(b"\0", off)
        return self.data[off:end].decode("utf-8", "replace")

    def section(self, name):
        for s in self.sections:
            if s["name"] == name:
                return s
        return None

    def body(self, s):
        return self.data[s["offset"]:s["offset"] + s["size"]]

    def dynamic(self):
        s = next((s for s in self.sections if s["type"] == SHT_DYNAMIC), None)
        if not s:
            return {}, [], None
        tags = defaultdict(list)
        b = self.body(s)
        for off in range(0, len(b) - 15, 16):
            tag, val = struct.unpack_from("<QQ", b, off)
            if tag == 0:
                break
            tags[tag].append(val)
        dynstr = self.sections[s["link"]] if s["link"] < len(self.sections) else self.section(".dynstr")
        needed = [self.cstr(dynstr["offset"] + v) for v in tags.get(1, [])] if dynstr else []
        return tags, needed, s

    def relocations(self):
        out = {"rela_dyn_entries": 0, "rela_relative": 0, "relr_relative": 0, "jmprel_entries": 0}
        rel_type = R_RELATIVE.get(self.e_machine)
        for s in self.sections:
            if s["type"] == SHT_RELA and s["name"] in (".rela.dyn",):
                b = self.body(s)
                n = len(b) // 24
                out["rela_dyn_entries"] += n
                if rel_type is not None:
                    for off in range(0, n * 24, 24):
                        info = struct.unpack_from("<Q", b, off + 8)[0]
                        if info & 0xFFFFFFFF == rel_type:
                            out["rela_relative"] += 1
            elif s["type"] == SHT_RELA and s["name"] == ".rela.plt":
                out["jmprel_entries"] += s["size"] // 24
            elif s["type"] == SHT_RELR or s["name"] == ".relr.dyn":
                b = self.body(s)
                for off in range(0, len(b) - 7, 8):
                    w = struct.unpack_from("<Q", b, off)[0]
                    out["relr_relative"] += 1 if w & 1 == 0 else bin(w >> 1).count("1")
        out["relative_total"] = out["rela_relative"] + out["relr_relative"]
        return out

    def symbols(self):
        symtab = next((s for s in self.sections if s["type"] == SHT_SYMTAB), None)
        if not symtab:
            return None
        strtab = self.sections[symtab["link"]]
        b = self.body(symtab)
        for off in range(0, len(b) - 23, 24):
            name, info, _, shndx, value, size = struct.unpack_from("<IBBHQQ", b, off)
            if size == 0 or shndx == 0 or shndx >= len(self.sections):
                continue
            yield self.cstr(strtab["offset"] + name), info & 0xF, self.sections[shndx]["name"], value, size


def report(path, crates):
    """Full report. `crates` holds every crate; main() truncates for output."""
    e = Elf(path)
    tags, needed, dynsec = e.dynamic()
    flags = tags.get(30, [0])[0]
    flags1 = tags.get(0x6FFFFFFB, [0])[0]
    alloc = {s["name"]: s["size"] for s in e.sections if s["flags"] & SHF_ALLOC and s["name"]}
    # sections_total counts bytes in the file: .bss and other SHT_NOBITS sections are excluded.
    file_bytes = sum(s["size"] for s in e.sections if s["flags"] & SHF_ALLOC and s["name"] and s["type"] != SHT_NOBITS)
    loads = [p for p in e.segments if p["type"] == PT_LOAD]
    has_interp = any(p["type"] == PT_INTERP for p in e.segments)
    r = {
        "file": path,
        "size": os.path.getsize(path),
        "arch": MACHINE.get(e.e_machine, hex(e.e_machine)),
        "type": ETYPE.get(e.e_type, str(e.e_type)),
        # ET_DYN with a program interpreter is a PIE executable; DF_1_PIE is set by newer linkers only.
        "pie": e.e_type == 3 and (has_interp or bool(flags1 & 0x08000000)),
        "stripped": e.section(".symtab") is None,
        "sections": alloc,
        "sections_total": file_bytes,
        "load_segments": {"count": len(loads), "filesz": sum(p["filesz"] for p in loads), "memsz": sum(p["memsz"] for p in loads)},
        "relocations": e.relocations(),
        "dynamic": {
            "needed": needed,
            "bind_now": bool(flags & 0x8) or bool(flags1 & 0x1) or 24 in tags,
            "flags": [n for bit, n in DF.items() if flags & bit],
            "flags_1": [n for bit, n in DF_1.items() if flags1 & bit],
            "relacount": tags.get(0x6FFFFFF9, [None])[0],
        },
        "crates": None,
    }
    if crates and not r["stripped"]:
        text = defaultdict(lambda: {"text": 0, "symbols": 0})
        data = defaultdict(int)
        covered = 0
        seen = set()
        for name, typ, sec, value, size in e.symbols():
            # Aliases (merged functions, C++ constructor variants) share one range; count it once.
            if (sec, value, size) in seen:
                continue
            seen.add((sec, value, size))
            if sec == ".text" and typ == 2:
                c = crate_of(name)
                text[c]["text"] += size
                text[c]["symbols"] += 1
                covered += size
            elif sec in (".rodata", ".data.rel.ro", ".data") and typ == 1:
                data[crate_of(name)] += size
        rows = sorted(text.items(), key=lambda kv: -kv[1]["text"])
        r["crates"] = [{"crate": c, "text": v["text"], "symbols": v["symbols"], "data": data.get(c, 0)} for c, v in rows]
        r["crates_total"] = len(rows)
        r["text_symbol_coverage"] = round(covered / alloc[".text"], 4) if alloc.get(".text") else None
    return r


def truncate(r, top):
    if r.get("crates") is not None:
        r["crates"] = r["crates"][:top]


def diff(a, b, top):
    """Deltas b - a over the full maps; only the crate list is cut to `top` rows."""
    d = {"size": b["size"] - a["size"], "sections": {}, "relocations": {}, "load_memsz": b["load_segments"]["memsz"] - a["load_segments"]["memsz"]}
    for k in sorted(set(a["sections"]) | set(b["sections"])):
        delta = b["sections"].get(k, 0) - a["sections"].get(k, 0)
        if delta:
            d["sections"][k] = delta
    for k in a["relocations"]:
        delta = b["relocations"][k] - a["relocations"][k]
        if delta:
            d["relocations"][k] = delta
    d["needed_added"] = sorted(set(b["dynamic"]["needed"]) - set(a["dynamic"]["needed"]))
    d["needed_removed"] = sorted(set(a["dynamic"]["needed"]) - set(b["dynamic"]["needed"]))
    d["bind_now"] = [a["dynamic"]["bind_now"], b["dynamic"]["bind_now"]]
    if a.get("crates") and b.get("crates"):
        ta = {c["crate"]: c["text"] for c in a["crates"]}
        tb = {c["crate"]: c["text"] for c in b["crates"]}
        rows = [(c, tb.get(c, 0) - ta.get(c, 0)) for c in set(ta) | set(tb)]
        rows = [(c, v) for c, v in rows if v]
        rows.sort(key=lambda cv: -abs(cv[1]))
        d["crates_text"] = [{"crate": c, "delta": v} for c, v in rows[:top]]
    return d


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("binary")
    ap.add_argument("--diff", metavar="OTHER", help="second binary; output has 'a' (OTHER, the baseline), 'b' (binary) and 'diff'")
    ap.add_argument("--top", type=int, default=20)
    ap.add_argument("--no-crates", action="store_true")
    args = ap.parse_args()
    b = report(args.binary, not args.no_crates)
    if args.diff:
        a = report(args.diff, not args.no_crates)
        out = {"a": a, "b": b, "diff": diff(a, b, args.top)}
        truncate(a, args.top)
    else:
        out = b
    truncate(b, args.top)
    json.dump(out, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
