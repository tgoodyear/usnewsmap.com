#!/usr/bin/env python3
"""Join one reader's per-set outputs into one hypothesis file for `gpu-study score`.

    join_hyps.py --reader-dir DIR --out hyps.jsonl [--filter-file IDS.jsonl] SET [SET ...]

DIR/SET/outputs.jsonl rows {"id" or "crop_id", "text"} become {"id": "SET/<crop id>", "text"}.
A set with no directory under DIR adds nothing, so its crops score as missing (empty) in the scorer.
--text-from-file reads rows from a single file instead: American Stories' Study 3 readings.
"""
import argparse, json
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--reader-dir", type=Path)
ap.add_argument("--single", type=Path, help="one outputs file for the one set named")
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("sets", nargs="+")
a = ap.parse_args()
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
