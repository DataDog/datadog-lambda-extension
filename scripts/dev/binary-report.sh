#!/usr/bin/env bash
# JSON report on a bottlecap ELF binary: file size, section sizes, RELATIVE
# relocation count, NEEDED libs, BIND_NOW, top crates by .text size.
#
#   scripts/dev/binary-report.sh <binary> [--diff <baseline>] [--top N] [--no-crates]
#
# Works on macOS and Linux without readelf or cargo bloat (pure Python ELF
# parser in binary_report.py). Crate attribution needs an unstripped binary;
# the CI job `bottlecap symbols (<arch>)` publishes one per main commit.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -I "$here/binary_report.py" "$@"
