#!/usr/bin/env python3
"""Writes a gold.py transcription file (crops.jsonl) from a transcription export of check.usnewsmap.com.

  curl -s 'https://check.usnewsmap.com/api/export?lang=de-968' > export.jsonl
  python3 export_gold_crops.py --export export.jsonl --template DIR/crops/crops.jsonl --out DIR/crops/crops.jsonl

Each template row whose task settled gets its `transcription` (the settled text, line ends LF), `transcriber`
("maintainer" for a single-reader set, else the settling method) and `reviewer` (null for single-reader sets, so
validate with `gold.py validate --allow-unreviewed` and say so wherever scores are quoted). Rows whose task has not settled
stay as in the template (transcription null). The template is copied to <out>.template once, never overwritten.
Standard library only.
"""
import argparse
import json
import os
import shutil
import sys


def rows(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]


def merge(template, export):
    by = {}
    for e in export:
        if e.get("settled") and e.get("text"):
            by[e.get("crop_id") or e["id"].rsplit("/", 1)[-1]] = e
    out, done = [], 0
    for r in template:
        e = by.get(r["crop_id"])
        r = dict(r)
        if e:
            r["transcription"] = e["text"]
            single = e.get("method") == "single"
            r["transcriber"] = "maintainer" if single else e.get("method")
            r["reviewer"] = None if single else "check.usnewsmap.com adjudication"
            done += 1
        out.append(r)
    return out, done


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--export", required=True)
    ap.add_argument("--template", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    keep = a.out + ".template"
    if not os.path.exists(keep):
        shutil.copyfile(a.template, keep)
    merged, done = merge(rows(keep), rows(a.export))
    with open(a.out, "w", encoding="utf-8") as f:
        for r in merged:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"{done} of {len(merged)} crops transcribed -> {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
