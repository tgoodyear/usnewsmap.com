#!/usr/bin/env python3
"""Cut the German band crops from the scan masters (#835, #968).

    german_band_crops.py --manifest MANIFEST.jsonl --masters DIR --out NEW_DIR

MANIFEST rows come from german_band_boxes.py. DIR holds each master as <page_id>.<jp2|jpg>; the
master's SHA-256 is checked against the row. Writes, as gold.py `cut` does: images/<crop_id>.jpg
(greyscale, JPEG quality 85, gold.py's encoder), manifest.jsonl and crops.jsonl (transcription null,
no hints). crop_id is de968-<band index>-<n>; the band is in `stratum`.
"""
import argparse, hashlib, json, sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "scripts" / "ocr-survey"))
import gold  # noqa: E402
import PIL
from PIL import Image

Image.MAX_IMAGE_PIXELS = None
ap = argparse.ArgumentParser()
ap.add_argument("--manifest", type=Path, required=True)
ap.add_argument("--masters", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
a = ap.parse_args()
if a.out.exists():
    sys.exit(f"--out must not exist: {a.out}")
(a.out / "images").mkdir(parents=True)
bands = ["<0.10", "0.10-0.20", "0.20-0.25", "0.25-0.32", ">=0.32"]
rows = [json.loads(x) for x in a.manifest.read_text().splitlines()]
rows.sort(key=lambda r: (bands.index(r["band"]), r["page_id"]))
count = {}
man, tpl = [], []
for r in rows:
    m = next(a.masters.glob(f"{r['page_id']}.*"))
    data = m.read_bytes()
    if hashlib.sha256(data).hexdigest() != r["master_sha256"]:
        sys.exit(f"master checksum differs: {r['page_id']}")
    img = Image.open(m)
    box = r["box"]
    assert list(img.size) == r["master_size"], (r["page_id"], img.size)
    i = bands.index(r["band"])
    count[i] = count.get(i, 0) + 1
    cid = f"de968-{i}-{count[i]:02d}"
    crop = gold.encode_crop(img, box)
    (a.out / "images" / f"{cid}.jpg").write_bytes(crop)
    man.append({"crop_id": cid, "page_id": r["page_id"], "group": "de968", "lang": "ger", "stratum": r["band"],
                "band": r["band"], "loc_damage": r["loc_damage"], "lccn": r["lccn"], "date": r["date"],
                "edition": r["edition"], "seq": r["seq"], "box": box, "image": f"images/{cid}.jpg",
                "crop_sha256": gold.sha256(crop), "crop_bytes": len(crop), "encoder": f"Pillow {PIL.__version__}",
                "page_image_sha256": r["master_sha256"], "master_size": r["master_size"]})
    tpl.append({"crop_id": cid, "group": "de968", "lang": "ger", "date": r["date"], "title": r["lccn"],
                "image": f"images/{cid}.jpg", "hints": {"is_truth": False, "note": "none", "texts": {}},
                "transcription": None, "transcriber": None, "reviewer": None, "hint_copy_reviewed": False, "notes": ""})
gold.write_jsonl(a.out / "manifest.jsonl", man)
gold.write_jsonl(a.out / "crops.jsonl", tpl)
print(len(man), "crops", sum(x["crop_bytes"] for x in man) / 1e6, "MB")
