"""Summarize a measure.py run: medians per variant, and differences between variants.

Usage:
  python3 summarize.py runs/<label>                      # each variant minus `noext`, if present
  python3 summarize.py runs/<label> --compare old new    # also `new` minus `old`

A difference is the difference of the two medians, with a bootstrap 95% confidence interval.
The bootstrap resamples whole execution environments, because invocations in one environment
are not independent. An interval that contains 0 means the run did not show a difference.
"""

import argparse
import json
import os
import random
import re
import statistics
from collections import defaultdict

REPORT_RE = re.compile(r"(Init Duration|Billed Duration|Duration|Max Memory Used): ([\d.]+)")


def parse(path):
    """Return one dict per cold start: its REPORT fields and the extension's cold start steps."""
    streams = defaultdict(list)
    with open(path) as f:
        for line in f:
            e = json.loads(line)
            streams[e["logStreamName"]].append(e)
    rows = []
    for events in streams.values():
        events.sort(key=lambda e: e["timestamp"])
        if not any(e["message"].startswith("INIT_START") for e in events):
            continue  # an execution environment from before this run
        row, warm = {}, []
        for e in events:
            m = e["message"]
            if '"cold_start":{' in m:
                steps = json.loads(m)["cold_start"]
                row.update({f"ext.{k}": v for k, v in steps.items()})
            elif m.startswith("REPORT"):
                fields = {k: float(v) for k, v in REPORT_RE.findall(m)}
                if "Init Duration" in fields:
                    row["init_duration_ms"] = fields["Init Duration"]
                    row["cold_duration_ms"] = fields["Duration"]
                    row["cold_max_memory_mb"] = fields["Max Memory Used"]
                else:
                    warm.append(fields["Duration"])
        if warm:
            row["warm_duration_ms"] = statistics.median(warm)
            row["_warm_durations"] = warm
        if "init_duration_ms" in row:
            rows.append(row)
    return rows


def boot_median_diff(base_rows, rows, key, n=2000):
    """Bootstrap 95% interval of the difference of the medians, resampling execution environments."""
    diffs = sorted(
        statistics.median(values(random.choices(rows, k=len(rows)), key))
        - statistics.median(values(random.choices(base_rows, k=len(base_rows)), key))
        for _ in range(n)
    )
    return diffs[int(0.025 * n)], diffs[int(0.975 * n)]


def has(row, key):
    return bool(row.get("_warm_durations")) if key == "each_warm_duration_ms" else key in row


def values(rows, key):
    # All warm invocations of the rows, for a run with few cold starts and many warm invocations.
    if key == "each_warm_duration_ms":
        return [x for r in rows for x in r.get("_warm_durations", [])]
    return [r[key] for r in rows if key in r]


def print_diff(title, base_rows, rows, keys):
    print(f"\n## {title}")
    print(f"{'metric':42} {'difference':>11} {'95% interval':>20}")
    for k in keys + ["each_warm_duration_ms"]:
        a, b = [r for r in base_rows if has(r, k)], [r for r in rows if has(r, k)]
        if len(a) < 3 or len(b) < 3:
            continue
        lo, hi = boot_median_diff(a, b, k)
        diff = statistics.median(values(b, k)) - statistics.median(values(a, k))
        print(f"{k:42} {diff:11.2f} {f'[{lo:.2f}, {hi:.2f}]':>20}")


def main():
    random.seed(0)
    p = argparse.ArgumentParser()
    p.add_argument("dir")
    p.add_argument("--baseline", default="noext", help="variant without the extension")
    p.add_argument("--compare", nargs=2, metavar=("OLD", "NEW"), action="append", default=[])
    args = p.parse_args()

    with open(os.path.join(args.dir, "meta.json")) as f:
        meta = json.load(f)
    data = {v: parse(os.path.join(args.dir, f"{v}.jsonl")) for v in meta["variants"]}
    keys = sorted({k for rows in data.values() for r in rows for k in r if not k.startswith("_")})

    print(f"{'variant':12} {'cold starts':>11} {'zipped':>10} {'unzipped':>10}")
    for v, rows in data.items():
        m = meta["variants"][v]
        print(f"{v:12} {len(rows):11d} {m['zipped_bytes']:10d} {m['unzipped_bytes']:10d}")

    print(f"\n## Medians")
    print(f"{'metric':42} " + " ".join(f"{v:>12}" for v in data))
    for k in keys:
        cells = []
        for rows in data.values():
            xs = values(rows, k)
            cells.append(f"{statistics.median(xs):12.2f}" if xs else f"{'-':>12}")
        print(f"{k:42} " + " ".join(cells))

    report_keys = ["init_duration_ms", "cold_duration_ms", "warm_duration_ms", "cold_max_memory_mb"]
    if args.baseline in data:
        for v, rows in data.items():
            if v != args.baseline:
                print_diff(f"{v} minus {args.baseline} (overhead of the extension)", data[args.baseline], rows, report_keys)
    for old, new in args.compare:
        print_diff(f"{new} minus {old}", data[old], data[new], keys)


if __name__ == "__main__":
    main()
