#!/usr/bin/env python3
"""Summarizes boinc/scripts/iris-bench.sh trials (#736) into the issue's results table.

  iris-bench-report.py --dir DIR [--reference LABEL]

Per label: whole-trial wall time (container start to exit, so startup is included), crops/min,
pipeline load ms, generate ms p50/p95 (first crop in each group reported separately), tokens,
truncations/empties, peak cgroup memory, mean GPU Render/3D busy and mean actual frequency over the
whole trial, mean CPU busy, and text parity with the reference trials (same crop, byte-identical).
A configuration is the label with its _rN repetition suffix removed.
"""
from __future__ import annotations

import argparse
import collections
import json
import re
import statistics
import sys
from pathlib import Path


def pct(values, q):
    values = sorted(values)
    if not values:
        return None
    return values[min(len(values) - 1, round(q * (len(values) - 1)))]


def gpu_means(path: Path):
    """intel_gpu_top -J emits a stream of objects, not a JSON array."""
    text = path.read_text()
    rows, depth, start = [], 0, None
    for i, ch in enumerate(text):
        if ch == "{":
            if depth == 0:
                start = i
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0 and start is not None:
                try:
                    rows.append(json.loads(text[start:i + 1]))
                except json.JSONDecodeError:
                    pass
    rows = rows[1:]  # the first sample covers an undefined interval
    busy = [r["engines"]["Render/3D"]["busy"] for r in rows if "engines" in r and "Render/3D" in r["engines"]]
    freq = [r["frequency"]["actual"] for r in rows if "frequency" in r]
    return (statistics.fmean(busy) if busy else None, statistics.fmean(freq) if freq else None, len(rows))


def cpu_mean(path: Path):
    used = []
    for line in path.read_text().splitlines():
        parts = line.split()
        if len(parts) >= 17 and parts[0].isdigit():
            used.append(100 - int(parts[14]))
    return statistics.fmean(used) if used else None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dir", type=Path, required=True)
    parser.add_argument("--reference", default="n1_r1")
    args = parser.parse_args()
    reference = {}
    ref_path = args.dir / f"{args.reference}.json"
    if ref_path.exists():
        for crop in json.loads(ref_path.read_text())["crops"]:
            reference[crop["page_sha256"]] = crop["text_sha256"]
    groups = collections.defaultdict(list)
    for path in sorted(args.dir.glob("*.json")):
        if path.name.endswith(".gpu.json"):
            continue
        groups[re.sub(r"_r\d+$", "", path.stem)].append(path)
    print("| Config | Trials | Crops/trial | Load ms (mean) | First-crop gen ms | Later-crop gen ms p50/p95 "
          "| End-to-end crops/min | Truncated/empty | Parity vs reference | Peak cgroup GiB | GPU busy % | GPU MHz | CPU % |")
    print("|---|---:|---:|---:|---:|---|---:|---|---|---:|---:|---:|---:|")
    for name, paths in groups.items():
        loads, first, later, rates, bad, same, total, peaks, busy, freq, cpu = ([] for _ in range(11))
        for path in paths:
            data = json.loads(path.read_text())
            times = (args.dir / f"{path.stem}.time.txt").read_text().split()
            wall = float(times[1]) - float(times[0])
            rates.append(len(data["crops"]) / wall * 60)
            loads += [g["load_ms"] for g in data["groups"]]
            for crop in data["crops"]:
                (first if crop["position_in_group"] == 0 else later).append(crop["generate_ms"])
                bad.append(crop["truncated"] or crop["empty"])
                if crop["page_sha256"] in reference:
                    total.append(1)
                    same.append(reference[crop["page_sha256"]] == crop["text_sha256"])
                peaks.append(crop["memory"]["cgroup_peak"] or 0)
            peaks.append(max(g["memory_after_release"]["cgroup_peak"] or 0 for g in data["groups"]))
            gpu = args.dir / f"{path.stem}.gpu.json"
            if gpu.exists():
                b, f, _ = gpu_means(gpu)
                busy.append(b)
                freq.append(f)
            vm = args.dir / f"{path.stem}.vmstat.txt"
            if vm.exists():
                cpu.append(cpu_mean(vm))
        def mean(values):
            values = [v for v in values if v is not None]
            return f"{statistics.fmean(values):.1f}" if values else "n/a"
        later_text = f"{pct(later, .5):.0f}/{pct(later, .95):.0f}" if later else "n/a"
        parity = f"{sum(same)}/{len(same)}" if same else "n/a"
        print(f"| {name} | {len(paths)} | {len(data['crops'])} | {mean(loads)} | {mean(first)} | {later_text} "
              f"| {mean(rates)} | {sum(bad)} | {parity} | {max(peaks) / 2**30:.2f} | {mean(busy)} | {mean(freq)} | {mean(cpu)} |")
    return 0


if __name__ == "__main__":
    sys.exit(main())
