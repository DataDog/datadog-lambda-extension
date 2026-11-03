"""Binary-size and ELF report for one flavor, against the latest `main` build.

GitLab job `binary report (<flavor>)` runs it after `bottlecap (<flavor>)`:

    python3 -I .gitlab/scripts/binary_report.py --suffix amd64

It downloads `.binaries/bottlecap-<suffix>` from the latest `main` run of the
`bottlecap symbols (<suffix>)` job with the job token, runs
`scripts/dev/binary-report.sh <new> --diff <baseline>`, and writes to
`.binary-report/`: `report.json` (the full diff), `report.md` (a short table,
also printed), and `metrics.txt` (OpenMetrics for the GitLab metrics report).

It never fails on a number. A file-size increase over `--warn-kib` prints a
`WARNING:` line and writes `warning.txt`. Exit 1 only for a missing new binary
or a report script failure. Local use:

    python3 -I .gitlab/scripts/binary_report.py --suffix arm64 \
        --new target/release/bottlecap --baseline /tmp/bottlecap-main
"""

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
# BINARY_REPORT_SH points at another checkout's copy, for local runs.
REPORT_SH = pathlib.Path(os.environ.get("BINARY_REPORT_SH", ROOT / "scripts" / "dev" / "binary-report.sh"))
SECTIONS = [".text", ".rodata", ".data.rel.ro", ".eh_frame", ".data", ".bss"]


def fetch_baseline(suffix, ref, dest):
    api = os.environ.get("CI_API_V4_URL")
    project = os.environ.get("CI_PROJECT_ID")
    token = os.environ.get("CI_JOB_TOKEN")
    if not (api and project and token):
        return None, "no CI_API_V4_URL / CI_PROJECT_ID / CI_JOB_TOKEN; no baseline"
    job = f"bottlecap symbols ({suffix})"
    url = (
        f"{api}/projects/{project}/jobs/artifacts/{urllib.parse.quote(ref, safe='')}"
        f"/raw/.binaries/bottlecap-{suffix}?job={urllib.parse.quote(job)}"
    )
    req = urllib.request.Request(url, headers={"JOB-TOKEN": token})
    try:
        with urllib.request.urlopen(req, timeout=120) as resp, open(dest, "wb") as out:
            shutil.copyfileobj(resp, out)
    except urllib.error.HTTPError as err:
        return None, f"HTTP {err.code} for {url}"
    except (urllib.error.URLError, OSError) as err:
        return None, f"{err} for {url}"
    return dest, f"{ref} build of job '{job}'"


def run_report(new, baseline):
    cmd = [str(REPORT_SH), str(new), "--no-crates"]
    if baseline:
        cmd += ["--diff", str(baseline)]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit(f"binary-report.sh failed with exit {proc.returncode}")
    return json.loads(proc.stdout)


def fmt_int(n):
    return f"{n:,}"


def fmt_delta(n, base=None):
    sign = "+" if n > 0 else ""
    text = f"{sign}{n:,}"
    if base:
        text += f" ({sign}{100.0 * n / base:.2f}%)"
    return text


def render(report, suffix, baseline_desc, warn_bytes):
    has_diff = "diff" in report
    b = report["b"] if has_diff else report
    a = report.get("a")
    d = report.get("diff", {})
    lines = [f"### bottlecap-{suffix}: size and ELF report", ""]
    if has_diff:
        lines += [f"Baseline: {baseline_desc}.", ""]
        lines += ["| metric | main | this build | delta |", "|---|---:|---:|---:|"]
        lines.append(
            f"| file size | {fmt_int(a['size'])} | {fmt_int(b['size'])} | {fmt_delta(d['size'], a['size'])} |"
        )
        for sec in SECTIONS:
            if sec in b["sections"] or sec in (a.get("sections") or {}):
                va = a["sections"].get(sec, 0)
                vb = b["sections"].get(sec, 0)
                lines.append(f"| {sec} | {fmt_int(va)} | {fmt_int(vb)} | {fmt_delta(vb - va, va)} |")
        lines.append(
            f"| load memsz | {fmt_int(a['load_segments']['memsz'])} | {fmt_int(b['load_segments']['memsz'])} "
            f"| {fmt_delta(d['load_memsz'], a['load_segments']['memsz'])} |"
        )
        ra = a["relocations"]["relative_total"]
        rb = b["relocations"]["relative_total"]
        lines.append(f"| RELATIVE relocations | {fmt_int(ra)} | {fmt_int(rb)} | {fmt_delta(rb - ra, ra)} |")
        na = ", ".join(a["dynamic"]["needed"])
        nb = ", ".join(b["dynamic"]["needed"])
        needed_delta = []
        if d.get("needed_added"):
            needed_delta.append("added " + ", ".join(d["needed_added"]))
        if d.get("needed_removed"):
            needed_delta.append("removed " + ", ".join(d["needed_removed"]))
        lines.append(f"| NEEDED | {na} | {nb} | {'; '.join(needed_delta) or 'same'} |")
        lines.append(
            f"| BIND_NOW / PIE | {a['dynamic']['bind_now']} / {a['pie']} | {b['dynamic']['bind_now']} / {b['pie']} | |"
        )
        top = sorted(d["sections"].items(), key=lambda kv: -abs(kv[1]))[:5]
        if top:
            lines += ["", "Largest section deltas: " + ", ".join(f"{k} {fmt_delta(v)}" for k, v in top)]
    else:
        lines += [f"No baseline ({baseline_desc}); absolute numbers only.", ""]
        lines += ["| metric | this build |", "|---|---:|"]
        lines.append(f"| file size | {fmt_int(b['size'])} |")
        for sec in SECTIONS:
            if sec in b["sections"]:
                lines.append(f"| {sec} | {fmt_int(b['sections'][sec])} |")
        lines.append(f"| load memsz | {fmt_int(b['load_segments']['memsz'])} |")
        lines.append(f"| RELATIVE relocations | {fmt_int(b['relocations']['relative_total'])} |")
        lines.append(f"| NEEDED | {', '.join(b['dynamic']['needed'])} |")
        lines.append(f"| BIND_NOW / PIE | {b['dynamic']['bind_now']} / {b['pie']} |")
    warning = None
    if has_diff and d["size"] > warn_bytes:
        warning = (
            f"WARNING: bottlecap-{suffix} grew by {fmt_delta(d['size'], a['size'])} bytes, "
            f"over the {warn_bytes // 1024} KiB threshold. Informational; check the section deltas above."
        )
        lines += ["", warning]
    return "\n".join(lines) + "\n", warning


def metrics(report, suffix):
    b = report["b"] if "diff" in report else report
    labels = f'{{flavor="{suffix}"}}'
    out = [
        f"bottlecap_binary_size_bytes{labels} {b['size']}",
        f"bottlecap_binary_text_bytes{labels} {b['sections'].get('.text', 0)}",
        f"bottlecap_binary_load_memsz_bytes{labels} {b['load_segments']['memsz']}",
        f"bottlecap_binary_relative_relocations{labels} {b['relocations']['relative_total']}",
    ]
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suffix", required=True, help="flavor suffix, for example amd64 or arm64")
    ap.add_argument("--new", help="binary to report on (default .binaries/bottlecap-<suffix>)")
    ap.add_argument("--baseline", help="baseline binary (default: download from the main symbols job)")
    ap.add_argument("--baseline-ref", default=os.environ.get("BASELINE_REF", "main"))
    ap.add_argument("--out", default=".binary-report")
    ap.add_argument("--warn-kib", type=int, default=int(os.environ.get("BINARY_SIZE_WARN_KIB", "256")))
    args = ap.parse_args()

    out_dir = pathlib.Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    new = pathlib.Path(args.new) if args.new else ROOT / ".binaries" / f"bottlecap-{args.suffix}"
    if not new.is_file():
        sys.exit(f"missing {new}: this job needs the artifact of `bottlecap ({args.suffix})`")

    if args.baseline:
        baseline, desc = pathlib.Path(args.baseline), f"local file {args.baseline}"
    else:
        baseline, desc = fetch_baseline(args.suffix, args.baseline_ref, out_dir / f"baseline-bottlecap-{args.suffix}")
    if baseline is None:
        print(f"no baseline: {desc}")

    report = run_report(new, baseline)
    (out_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    table, warning = render(report, args.suffix, desc, args.warn_kib * 1024)
    (out_dir / "report.md").write_text(table)
    (out_dir / "metrics.txt").write_text(metrics(report, args.suffix))
    if warning:
        (out_dir / "warning.txt").write_text(warning + "\n")
    if baseline is not None and not args.baseline:
        baseline.unlink()
    print(table)


if __name__ == "__main__":
    main()
