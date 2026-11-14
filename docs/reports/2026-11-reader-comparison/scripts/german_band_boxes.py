#!/usr/bin/env python3
"""Crop boxes for the German band pages (#835, #968), from LoC's ALTO and the scan masters' sizes.

    german_band_boxes.py --resolved RESOLVED.jsonl --alto-dir DIR --ocr-survey DIR --out manifest.jsonl [--seed 296]

RESOLVED rows: the band selection plus path, sha256, width and height of the master on disk. The box
is `crops.pick_box` (gold.py cut's rule, seeded per page as gold.py does) on the page's ALTO. Pillow
isn't needed: the crops module's only use of it is cutting, which this script doesn't do.
"""
import argparse, json, random, sys, types
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--resolved", type=Path, required=True)
ap.add_argument("--alto-dir", type=Path, required=True)
ap.add_argument("--ocr-survey", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--seed", type=int, default=296)
a = ap.parse_args()
try:
    import PIL  # noqa: F401
except ImportError:
    pil = types.ModuleType("PIL"); pil.Image = types.ModuleType("PIL.Image")
    sys.modules["PIL"] = pil; sys.modules["PIL.Image"] = pil.Image
sys.path.insert(0, str(a.ocr_survey))
import crops  # noqa: E402

out, skipped = [], []
for line in a.resolved.read_text().splitlines():
    s = json.loads(line)
    pid = s["page_id"]
    alto = a.alto_dir / f"{pid}.xml"
    if not s.get("path") or not alto.exists():
        skipped.append((pid, "no master" if not s.get("path") else "no ALTO")); continue
    words = crops.alto_words(alto, s["width"])
    box = crops.pick_box(words, (s["width"], s["height"]), random.Random(f"{a.seed}:gold-box:{pid}"))
    if box is None:
        skipped.append((pid, "no text region of 600+ pixels")); continue
    out.append({"band": s["band"], "page_id": pid, "lccn": s["lccn"], "date": s["date"], "edition": s["edition"],
                "seq": s["seq"], "loc_damage": s["loc_damage"], "master_path": s["path"], "master_sha256": s["sha256"],
                "master_size": [s["width"], s["height"]], "box": list(box), "alto": f"{pid}.xml",
                "iiif_base": s.get("iiif_base")})
a.out.write_text("".join(json.dumps(r) + "\n" for r in out), encoding="utf-8")
print(len(out), "boxes; skipped:", skipped)
