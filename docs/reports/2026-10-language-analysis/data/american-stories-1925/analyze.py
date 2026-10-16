#!/usr/bin/env python3
"""Verify retained #327 inputs and regenerate grouped tables, offline (stdlib only)."""

import collections
import csv
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "ja-ocr"))
import american_stories as ams  # noqa: E402
import quality  # noqa: E402


def write_csv(name, fields, rows):
    buf = io.StringIO(newline="")
    writer = csv.DictWriter(buf, fieldnames=fields, lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
    data = buf.getvalue().encode()
    (HERE / name).write_bytes(gzip.compress(data, mtime=0) if name.endswith(".gz") else data)


def main():
    provenance = json.loads((HERE / "provenance.json").read_text())
    for name, expected in provenance["input_files"].items():
        data = (HERE / name).read_bytes()
        assert len(data) == expected["bytes"], name
        assert hashlib.sha256(data).hexdigest() == expected["sha256"], name
    with gzip.open(HERE / "sample.csv.gz", "rt") as source:
        pages = list(csv.DictReader(source))
    with gzip.open(HERE / "upstream-scan-names.txt.gz", "rt") as source:
        names = source.read().splitlines()
    with gzip.open(HERE / "imported-doc-ids.txt.gz", "rt") as source:
        imported_list = source.read().splitlines()
    imported = set(imported_list)
    assert len(imported) == len(imported_list) == 91788
    placed, placement = ams.place(names)
    assert set(placed.values()) == imported
    assert len(names) == 95839 and placement["unparsed"] == 0
    assert len({p["doc_id"] for p in pages}) == len(pages) == 18187
    issues = collections.defaultdict(list)
    labels = collections.defaultdict(list)
    physical = set()
    for name in names:
        lccn, day, edition, image, page = ams.parse(name)
        reel = ams.NAME.match(name.rsplit("/", 1)[-1]).group(4)
        issue = (lccn, day, edition)
        issues[issue].append(name)
        physical.add((*issue, reel, image))
        if page is not None:
            labels[(*issue, page)].append(name)
    assert len(physical) == len(names)
    collisions = {key: value for key, value in labels.items() if len(value) > 1}
    collision_issues = {key[:3] for key in collisions}
    batches = {re.sub(r"_ver\d+$", "", b["batch"]): b
               for b in json.loads((HERE / "loc-batches.json").read_text())}
    counters = {name: collections.defaultdict(collections.Counter)
                for name in ["batch", "title", "date", "ingestion-year", "batch-title-date"]}
    detail = []
    totals = collections.Counter()
    for page in pages:
        doc_id = page["doc_id"]
        lccn, day, ed, seq = doc_id.split("_")
        issue = (lccn, day, int(ed[3:]))
        assert page["date"] == day and day.startswith("1925-")
        assert page["lccn"] == lccn and quality.sampled(doc_id, 1000)
        assert page["page_key"] == "/".join([lccn, day, ed, seq])
        matched = doc_id in imported
        assert page["imported"] == str(int(matched))
        status = ("matched" if matched else "issue_absent" if issue not in issues else
                  "label_missing_collision_issue" if issue in collision_issues else
                  "label_missing_other_issue")
        count = collections.Counter(sampled=1, matched=int(matched), missing=int(not matched))
        if not matched:
            count[status] = 1
        count["matched_collision_issue"] = int(matched and issue in collision_issues)
        totals.update(count)
        ingested = batches[page["batch"]]["ingested"]
        keys = {"batch": (page["batch"], ingested), "title": (lccn, page["title"]),
                "date": (day,), "ingestion-year": (ingested[:4],),
                "batch-title-date": (page["batch"], lccn, page["title"], day)}
        for dimension, key in keys.items():
            counters[dimension][key].update(count)
        detail.append({**page, "status": status, "collision_issue": int(issue in collision_issues),
                       "loc_batch_ingested": ingested})
    count_fields = ["sampled", "matched", "missing", "issue_absent", "label_missing_collision_issue",
                    "label_missing_other_issue", "matched_collision_issue"]
    dimensions = {"batch": ["batch", "loc_batch_ingested"], "title": ["lccn", "title"],
                  "date": ["date"], "ingestion-year": ["loc_batch_ingestion_year"],
                  "batch-title-date": ["batch", "lccn", "title", "date"]}
    for dimension, keys in dimensions.items():
        rows = [{**dict(zip(keys, key)), **{f: counts[f] for f in count_fields}}
                for key, counts in sorted(counters[dimension].items())]
        for field in count_fields:
            assert sum(row[field] for row in rows) == totals[field]
        suffix = ".csv.gz" if dimension == "batch-title-date" else ".csv"
        write_csv("by-" + dimension + suffix, keys + count_fields, rows)
    write_csv("classified-sample.csv.gz", list(detail[0]), detail)
    write_csv("identifier-collisions.csv.gz", ["lccn", "date", "edition", "page_label", "scans", "names"],
              [dict(lccn=l, date=d, edition=e, page_label=p, scans=len(ns), names="|".join(ns))
               for (l, d, e, p), ns in sorted(collisions.items())])
    assert totals["matched"] == 8597 and totals["missing"] == 9590
    assert totals["issue_absent"] == 8811
    assert totals["label_missing_collision_issue"] == 323
    assert totals["label_missing_other_issue"] == 456
    summary = {"reference_version": provenance["reference_version"], **dict(totals),
               "coverage_pct": round(100 * totals["matched"] / totals["sampled"], 4),
               "upstream_physical_scans": len(physical), "imported_page_ids": len(imported),
               "placement": placement, "colliding_numbered_labels": len(collisions),
               "collision_issues": len(collision_issues),
               "excess_scans_at_numbered_labels": sum(len(v) - 1 for v in collisions.values()),
               "groups": {k: len(v) for k, v in counters.items()}}
    (HERE / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(json.dumps(summary, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
