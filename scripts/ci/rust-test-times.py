#!/usr/bin/env python3
"""Summarize stable libtest's per-binary durations without changing test execution."""
import argparse
from pathlib import Path
import re


def timings(text):
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    binary = None
    rows = []
    for line in text.splitlines():
        running = re.search(r"\b(Running|Doc-tests)\s+(.+)", line)
        if running:
            binary = running[2].strip()
        result = re.search(r"test result: (\w+)\..*finished in ([\d.]+)s", line)
        if result and binary:
            rows.append((float(result[2]), result[1], binary))
            binary = None
    return sorted(rows, reverse=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", type=Path, required=True)
    args = parser.parse_args()
    rows = timings(args.log.read_text())
    if not rows:
        parser.error("no completed Rust test binaries found in log")
    print("| Seconds | Result | Test binary |")
    print("|---:|---|---|")
    for seconds, result, binary in rows:
        print(f"| {seconds:.2f} | {result} | `{binary}` |")


if __name__ == "__main__":
    main()
