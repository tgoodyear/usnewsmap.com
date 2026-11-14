#!/usr/bin/env python3
"""Builds proxy-rows input: one row per (reader, crop) from the frozen #968 refs and the joined readings.

    prep_rows.py --dir /tank/usnm-lake/968-dev2-20261112 --out rows-in.jsonl

Reads DIR/refs.jsonl and DIR/hyps/{tess3,tess6,xiaomi-local,qwen8b-local,loc}.jsonl (read-only).
Out rows: {"id": "<reader>|<crop id>", "lang", "ref", "hyp"}. A crop a reader has no row for is left out.
"""
import argparse, json
from pathlib import Path

READERS = ["tess3", "tess6", "xiaomi-local", "qwen8b-local", "loc"]
ap = argparse.ArgumentParser()
ap.add_argument("--dir", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
a = ap.parse_args()
refs = [json.loads(l) for l in (a.dir / "refs.jsonl").read_text(encoding="utf-8").splitlines() if l.strip()]
with a.out.open("w", encoding="utf-8") as out:
    for rd in READERS:
        hyp = {}
        for l in (a.dir / "hyps" / f"{rd}.jsonl").read_text(encoding="utf-8").splitlines():
            if l.strip():
                r = json.loads(l)
                hyp[r["id"]] = r["text"]
        for r in refs:
            if r["id"] in hyp:
                out.write(json.dumps({"id": f"{rd}|{r['id']}", "lang": r["lang"], "ref": r["ref"], "hyp": hyp[r["id"]]}, ensure_ascii=False) + "\n")
