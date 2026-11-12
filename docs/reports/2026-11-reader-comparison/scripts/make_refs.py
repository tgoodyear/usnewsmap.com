#!/usr/bin/env python3
"""References for `gpu-study score`, from gold.py-format sets.

    make_refs.py --set NAME=DIR [--set NAME=DIR ...] --out refs.jsonl

One row {"id", "page", "ref", "source", "lang"} per crop: id is the crop id prefixed by the set
name (so ids are unique across sets), page is the manifest's page_id (what the bootstrap resamples),
source is the set name, ref is the human transcription.
"""
import argparse, json
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--set", action="append", required=True, help="NAME=DIR")
ap.add_argument("--out", type=Path, required=True)
a = ap.parse_args()
rows = []
for spec in a.set:
    name, d = spec.split("=", 1)
    d = Path(d)
    man = {r["crop_id"]: r for r in map(json.loads, d.joinpath("manifest.jsonl").read_text(encoding="utf-8").splitlines())}
    for t in map(json.loads, d.joinpath("crops.jsonl").read_text(encoding="utf-8").splitlines()):
        m = man[t["crop_id"]]
        rows.append({"id": f"{name}/{t['crop_id']}", "page": f"{name}/{m['page_id']}", "ref": t["transcription"],
                     "source": name, "lang": t["lang"]})
a.out.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
print(len(rows), "rows")
