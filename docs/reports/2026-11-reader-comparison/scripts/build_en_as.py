#!/usr/bin/env python3
"""Build en-as: the American Stories hand-transcribed gold pages, one crop per non-empty region, in gold.py layout.

    build_en_as.py --src DIR/as-gold-20261113 --gold-py scripts/ocr-survey --out SETDIR
"""
import argparse, json, re, sys
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--src", type=Path, required=True)
ap.add_argument("--scripts", type=Path, required=True, help="scripts/ocr-survey")
ap.add_argument("--out", type=Path, required=True)
a = ap.parse_args()
sys.path.insert(0, str(a.scripts))
import gold  # noqa: E402
import PIL  # noqa: E402
from PIL import Image  # noqa: E402

Image.MAX_IMAGE_PIXELS = None
CATS = {0: "headline", 1: "article", 2: "newspaper_header", 3: "masthead", 4: "author", 5: "photograph", 6: "image_caption",
        7: "page_number", 8: "table", 9: "cartoon_or_advertisement"}
URL = "https://huggingface.co/datasets/dell-research-harvard/AmericanStoriesTraining"
LICENSE = ("Apache-2.0 (Hugging Face repository metadata; the card is empty). The page images are Chronicling America "
           "scans from the Library of Congress, public domain")
ATTR = ("Hand transcriptions from the American Stories gold set (Dell et al. 2023, arXiv:2308.12477; two annotators, "
        "discrepancies resolved), dell-research-harvard/AmericanStoriesTraining gold_data; page images from the "
        "Library of Congress, Chronicling America")
if a.out.exists():
    sys.exit(f"--out must not exist: {a.out}")
d = json.loads((a.src / "gold_data/gold_hand_transcription.json").read_text(encoding="utf-8"))
prov = {p["file"]: p for p in json.loads((a.src / "provenance.json").read_text())}
files = {i["id"]: i for i in d["images"]}
regions = []
for an in d["annotations"]:
    text = (an.get("text") or "")
    if not text.strip():
        continue
    im = files[an["image_id"]]
    x, y, w, h = an["bbox"]
    regions.append((im["file_name"], an["id"], [int(x), int(y), int(x + w + 0.999), int(y + h + 0.999)], an["category_id"], text.strip("\n")))
regions.sort(key=lambda r: (r[0], r[1]))
(a.out / "images").mkdir(parents=True)
(a.out / "LICENSES").mkdir()
manifest, template, page_sha, cache = [], [], {}, {}
for n, (fn, rid, box, cat, text) in enumerate(regions, 1):
    p = prov[fn]
    pid = f"{p['lccn']}_{p['date']}_ed-{p['edition']}_seq-{p['seq']}"
    path = a.src / "gold_data/gold_images" / fn
    if fn not in page_sha:
        page_sha[fn] = gold.sha256(path.read_bytes())
        cache.clear()
        cache[fn] = Image.open(path)
        cache[fn].load()
    img = cache[fn]
    b = [max(0, box[0]), max(0, box[1]), min(img.width, box[2]), min(img.height, box[3])]
    assert b[2] > b[0] and b[3] > b[1], (fn, rid, box)
    data = gold.encode_crop(img, b)
    cid = f"en-as-{n:03d}"
    nchars = len(gold.normalize(text, truth=True))
    band = "<100" if nchars < 100 else ("100-500" if nchars <= 500 else ">500")
    (a.out / "images" / f"{cid}.jpg").write_bytes(data)
    manifest.append({"crop_id": cid, "page_id": pid, "group": "en-as", "lang": "eng", "era": None, "stratum": "en-as",
        "box": b, "image": f"images/{cid}.jpg", "crop_sha256": gold.sha256(data), "crop_bytes": len(data),
        "encoder": f"Pillow {PIL.__version__}", "page_image_sha256": page_sha[fn], "source_image": fn,
        "region": f"{fn[:-4]}/{rid}", "category": CATS[cat], "ref_chars": nchars, "length_band": band,
        "master_size": [img.width, img.height], "source_dataset": "americanstories-gold", "source_url": URL,
        "source_license": LICENSE, "source_attribution": ATTR, "for_scoring": True,
        "page_url": f"https://www.loc.gov/resource/{p['lccn']}/{p['date']}/ed-{p['edition']}/?sp={p['seq']}",
        "loc_batch": p["batch"]})
    template.append({"crop_id": cid, "group": "en-as", "lang": "eng", "date": p["date"], "title": p["title"],
        "image": f"images/{cid}.jpg", "hints": {"is_truth": False, "note": "none: the truth is the dataset's own", "texts": {}},
        "transcription": text, "transcriber": "americanstories-gold: two annotators, discrepancies resolved (Dell et al. 2023)",
        "reviewer": "not applicable: published ground truth", "hint_copy_reviewed": False,
        "truth_source": "americanstories-gold", "for_scoring": True, "notes": ""})
gold.write_jsonl(a.out / "manifest.jsonl", manifest)
gold.write_jsonl(a.out / "crops.jsonl", template)
for f in ("README.md",):
    (a.out / "LICENSES" / "README-americanstories.md").write_text((a.src / f).read_text())
(a.out / "LICENSES/SOURCE.json").write_text(json.dumps({"dataset": "americanstories-gold", "url": URL, "license": LICENSE,
    "attribution": ATTR, "policy": "passes: pages identified as Chronicling America by text match to LoC OCR (provenance.json)",
    "files": ["gold_data/gold_hand_transcription.json", "gold_data/gold_images/*.jpg"]}, indent=1, ensure_ascii=False) + "\n")
(a.out / "provenance.json").write_bytes((a.src / "provenance.json").read_bytes())
tot = sum(m["crop_bytes"] for m in manifest)
big = max(manifest, key=lambda m: m["crop_bytes"])
print(json.dumps({"crops": len(manifest), "pages": len(page_sha), "chars": sum(m["ref_chars"] for m in manifest),
    "crop_bytes_total": tot, "crop_bytes_max": big["crop_bytes"], "largest": big["crop_id"],
    "bands": {k: sum(1 for m in manifest if m["length_band"] == k) for k in ("<100", "100-500", ">500")}}))
