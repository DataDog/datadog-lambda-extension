import re
import statistics
import sys
from typing import Iterable, Optional, Tuple

"""
This file computes statistics from the logs of the Lambda function.
It parses the REPORT lines to extract all available information.
"""

Report = Tuple[str, float, int, int, int, Optional[float]]
REPORT_RE = re.compile(
    r"^REPORT"
    r"\s+RequestId: (?P<request_id>.+)"
    r"\s+Duration: (?P<duration>.+) ms"
    r"\s+Billed Duration: (?P<billed_duration>.+) ms"
    r"\s+Memory Size: (?P<memory_size>.+) MB"
    r"\s+Max Memory Used: (?P<max_memory_used>.+) MB"
    r"(\s+Init Duration: (?P<init_duration>.+) ms)?"
    r"\s*$"
)

GO_INIT_RE = re.compile(
    r"init github.com/DataDog/datadog-agent/pkg/serverless/trace @(?P<init_duration>\d+) ms"
)


def parse_reports(log_lines: Iterable[str]) -> Iterable[Report]:
    for log_line in log_lines:
        res = REPORT_RE.match(log_line)
        if res is None:
            continue

        init_duration = res.group("init_duration")
        if init_duration is not None:
            init_duration = float(init_duration)

        yield (
            res.group("request_id"),
            float(res.group("duration")),
            int(res.group("billed_duration")),
            int(res.group("memory_size")),
            int(res.group("max_memory_used")),
            init_duration,
        )


def parse_go_init(log_lines: Iterable[str]):
    for log_line in log_lines:
        res = GO_INIT_RE.match(log_line)
        if res is None:
            continue

        init_duration = res.group("init_duration")
        if init_duration is not None:
            init_duration = int(init_duration)

        yield init_duration


def print_stats(values, name, unit):
    mean = sum(values) / len(values)
    median = statistics.median(values)
    print(
        f"{name}: min={min(values)}{unit}, max={max(values)}{unit}, avg={mean}{unit}, median={median}{unit}"
    )


if __name__ == "__main__":
    with open(sys.argv[1], "r") as f:
        log_reports = f.readlines()

    durations = []
    memories = []
    init_durations = []

    for _, duration, _, _, mem, init in parse_reports(log_reports):
        durations.append(duration)
        memories.append(mem)

        if init is not None:
            init_durations.append(init)
        else:
            print("REPORT did not contain init duration", file=sys.stderr)

    print_stats(memories, "Memory", " MB")
    print_stats(durations, "Duration", " ms")
    print_stats(init_durations, "Init Duration", " ms")

    with open(sys.argv[2], "r") as f:
        goinit_reports = f.readlines()

        init_duirations = []

        for init in parse_go_init(goinit_reports):
            init_duirations.append(init)

        print_stats(init_duirations, "Go init", " ms")
