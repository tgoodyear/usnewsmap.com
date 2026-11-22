#!/usr/bin/env python3
"""Join one reader's per-set outputs into one hypothesis file for `gpu-study score`.

    join_hyps.py --reader-dir DIR --out hyps.jsonl --set SET [--set SET ...]

DIR/SET/outputs.jsonl rows {"id" or "crop_id", "text"} become {"id": "SET/<crop id>", "text"}.
A set with no directory under DIR adds nothing, so its crops score as missing (empty) in the scorer.
--single FILE reads the rows of the one set named from that file instead: American Stories' Study 3 readings.
"""
import argparse, json
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--reader-dir", type=Path)
ap.add_argument("--single", type=Path, help="one outputs file for the one set named")
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--set", dest="sets", action="append", required=True, help="a set name; repeat for each set")
a = ap.parse_args()
if a.single and len(a.sets) != 1:
    ap.error("--single takes exactly one --set")
if not a.single and not a.reader_dir:
    ap.error("--reader-dir is required without --single")
rows = []
for s in a.sets:
    p = a.single if a.single else a.reader_dir / s / "outputs.jsonl"
    if not p.exists():
        continue
    for line in p.read_text(encoding="utf-8").splitlines():
        if line.strip():
            r = json.loads(line)
            rows.append({"id": f"{s}/{r.get('crop_id') or r['id']}", "text": r["text"]})
a.out.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
print(len(rows), "rows")
