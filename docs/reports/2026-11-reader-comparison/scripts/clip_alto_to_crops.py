#!/usr/bin/env python3
"""A page-level ALTO clipped to each crop box of a gold.py-format set, as scorer hypotheses (#968, #835).

    clip_alto_to_crops.py --manifest DIR/manifest.jsonl --alto-dir DIR --out FILE.jsonl --alto-crop-bin usnm-ocr-survey

Used for LoC's ALTO and for our live frak2021 ALTO on the German band crops. Rows have `box` and
`master_size` ([width, height] of the page image the ALTO's coordinates belong to); the ALTO is
<page_id>.xml under --alto-dir. A failed crop gets empty text. Uses loc_column.alto_crop.
"""
import argparse, json, sys, tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "scripts" / "ocr-survey"))
import loc_column as lc  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--manifest", type=Path, required=True)
ap.add_argument("--alto-dir", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--alto-crop-bin", default="usnm-ocr-survey")
a = ap.parse_args()
out, bad = [], []
with tempfile.TemporaryDirectory() as t:
    empty = Path(t) / "e.txt"
    empty.write_text("")
    for line in a.manifest.read_text().splitlines():
        r = json.loads(line)
        try:
            res = lc.alto_crop(a.alto_crop_bin, a.alto_dir / f"{r['page_id']}.xml", r["box"], tuple(r["master_size"]), empty)
            out.append({"crop_id": r["crop_id"], "id": r["crop_id"], "text": res["alto_text"], "clipped_words": res.get("clipped_words")})
        except Exception as e:  # noqa: BLE001
            bad.append((r["crop_id"], str(e)[:100]))
            out.append({"crop_id": r["crop_id"], "id": r["crop_id"], "text": ""})
a.out.write_text("".join(json.dumps(o, ensure_ascii=False) + "\n" for o in out), encoding="utf-8")
print(len(out), "crops; failed", len(bad), "; empty", sum(1 for o in out if not o["text"].strip()))
