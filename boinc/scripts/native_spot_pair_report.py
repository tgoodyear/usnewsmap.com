#!/usr/bin/env python3
"""Distribution of a plan's native spot pairs (Science Coordinator's ruling of 16 Nov 2026).

The controller appends one JSON line for every compared pair, passes included, to
`native-spot-pairs.jsonl` in the plan's state directory (`native_spot_pair` events carry the same
fields in the journal). This reads that file and, once it holds at least `--min-pairs` pairs
(100 by default), prints or writes the distribution: pairs by status and by host, and the
min, 10th percentile, median, 90th, 99th and max of CER, word F1 and length ratio, with the
share of pairs outside the cross-build limits and how many had a peer that failed the completeness
gate. Below that it says how many pairs there are and exits 0 without a report.

    native_spot_pair_report.py --pairs /var/lib/usnm-priority-controller.native-feed-8/native-spot-pairs.jsonl
    native_spot_pair_report.py --pairs FILE --out report.json   # writes only once n >= 100
"""
import argparse
import json
import math
import sys
from collections import Counter
from pathlib import Path

MIN_PAIRS = 100
PERCENTILES = (("min", 0), ("p10", 10), ("p50", 50), ("p90", 90), ("p99", 99), ("max", 100))


def read_pairs(path):
    """The `native_spot_pair` records in `path`, in order, one for each replay: a line that isn't
    one is skipped, and so is a repeat of a pair (the same job, peer host and reading), which a
    controller stopped between the store and its state can write again."""
    pairs, seen = [], set()
    for line in Path(path).read_text().splitlines():
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if not (isinstance(record, dict) and record.get("event") == "native_spot_pair"):
            continue
        key = (record.get("job"), record.get("peer_host"), bool(record.get("third_reading")))
        if key not in seen:
            seen.add(key)
            pairs.append(record)
    return pairs


def percentile(values, q):
    """The `q`th percentile of the sorted `values` by linear interpolation."""
    if not values:
        return None
    if len(values) == 1:
        return values[0]
    rank = (len(values) - 1) * q / 100
    low, high = math.floor(rank), math.ceil(rank)
    return values[low] + (values[high] - values[low]) * (rank - low)


def distribution(values):
    values = sorted(v for v in values if isinstance(v, (int, float)))
    return {name: percentile(values, q) for name, q in PERCENTILES} | {"n": len(values)}


def report(pairs, min_pairs=MIN_PAIRS):
    """The distribution, or None while there are fewer than `min_pairs` pairs."""
    if len(pairs) < min_pairs:
        return None
    hosts = {}
    for host in sorted({str(p["source_host"]) for p in pairs}):
        mine = [p for p in pairs if str(p["source_host"]) == host]
        hosts[host] = {"pairs": len(mine), "outside": sum(p["status"] == "outside" for p in mine),
                       "first_readings": sum(not p.get("third_reading") for p in mine)}
    return {"pairs": len(pairs),
            "first_at": min(p["at"] for p in pairs), "last_at": max(p["at"] for p in pairs),
            "by_status": dict(sorted(Counter(p["status"] for p in pairs).items())),
            "third_readings": sum(bool(p.get("third_reading")) for p in pairs),
            "outside_share": sum(p["status"] == "outside" for p in pairs) / len(pairs),
            "peer_gate_failed": sum(p.get("peer_gate_pass") is False for p in pairs),
            "word_f1_under_half": sum(isinstance(p.get("word_f1"), (int, float)) and p["word_f1"] < 0.5
                                      for p in pairs),
            "hosts": hosts,
            "cer": distribution(p.get("cer") for p in pairs),
            "word_f1": distribution(p.get("word_f1") for p in pairs),
            "length_ratio": distribution(p.get("length_ratio") for p in pairs),
            "peer_damage_rate": distribution(p.get("peer_damage_rate") for p in pairs)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pairs", type=Path, required=True, help="the plan's native-spot-pairs.jsonl")
    parser.add_argument("--min-pairs", type=int, default=MIN_PAIRS, help="pairs needed before a report (100)")
    parser.add_argument("--out", type=Path, help="write the report here (JSON) instead of printing it")
    args = parser.parse_args(argv)
    pairs = read_pairs(args.pairs)
    result = report(pairs, args.min_pairs)
    if result is None:
        print(f"{len(pairs)} pairs; a report needs {args.min_pairs}", file=sys.stderr)
        return 0
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.write_text(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
