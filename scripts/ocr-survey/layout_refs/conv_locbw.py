#!/usr/bin/env python3
"""LoC Beyond Words -> common layout-reference rows (#2107).

Dataset: biglam/loc_beyond_words, CC0-1.0, https://huggingface.co/datasets/biglam/loc_beyond_words
  3,558 Chronicling America pages (WWI era, English) with human-verified boxes for Photograph,
  Illustration, Map, Comics/Cartoon, Editorial Cartoon, Headline and Advertisement, and the
  canonical 80/20 split (train_80_percent.json 2,846 pages, val_20_percent.json 712 pages).
  Paper: Lee et al., "The Newspaper Navigator Dataset: Extracting Headlines and Visual Content from
  16 Million Historic Newspaper Pages in Chronicling America", CIKM 2020, arXiv:2005.01583,
  doi 10.1145/3340531.3412767. Project: https://labs.loc.gov/work/experiments/beyond-words/

Annotations are read from the two COCO JSON files (the canonical split), pages from images.zip
(image_path is "<images.zip>!images/<file_name>", nothing is copied). The parquet copies are only
used to cross-check object counts per image (--check-parquet).
The card says Headline and Advertisement boxes were added by the project team and are not all
crowd-verified.

Mapping:
  nontext_regions  Photograph, Illustration, Map, Comics/Cartoon, Editorial Cartoon, Advertisement.
  text_regions     Headline (type headline). There is no body-text label, so gutters and
                   column_gutters are empty and reading order and articles do not exist.
  meta.ca_identity {lccn, reel, date, edition, frame, batch, year} parsed from the source image
                   URL .../<batch>/data/<lccn>/<reel>/<yyyymmdd><edition 2 digits>/<frame>.jpg.
                   "frame" is the image number within the reel, NOT the issue's page sequence, so a
                   later join to LoC ALTO goes by LCCN + date + edition and then by page match.

    conv_locbw.py --data-dir DIR --out-dir DIR
"""
import argparse
import json
import os
import re
import sys
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402

DATASET = "biglam/loc_beyond_words"
DIRNAME = "biglam__loc_beyond_words"
LICENCE = "CC0-1.0"
CATEGORIES = ["Photograph", "Illustration", "Map", "Comics/Cartoon", "Editorial Cartoon", "Headline",
              "Advertisement"]
URL = re.compile(r"/([^/]+)/data/([^/]+)/([^/]+)/(\d{4})(\d{2})(\d{2})(\d{2})/(\d+)\.[A-Za-z]+$")
SPLITS = {"train": "train_80_percent.json", "val": "val_20_percent.json"}


def parse_identity(url):
    """Chronicling America identity from the source URL, or None."""
    m = URL.search(url or "")
    if not m:
        return None
    batch, lccn, reel, y, mo, d, ed, frame = m.groups()
    return {"batch": batch, "date": "%s-%s-%s" % (y, mo, d), "edition": int(ed),
            "frame": int(frame), "lccn": lccn, "reel": reel, "year": int(y)}


def row_from_coco(img, anns, split, image_path):
    w, h = int(img["width"]), int(img["height"])
    text_regions, nontext, problems = [], [], []
    for a in sorted(anns, key=lambda a: a["id"]):
        c = a["category_id"]
        if not 0 <= c < len(CATEGORIES):
            problems.append("annotation %s category %s" % (a["id"], c))
            continue
        x, y, bw, bh = a["bbox"]
        box = common.clamp_box([x, y, x + bw, y + bh], w, h)
        if not (box[2] > box[0] and box[3] > box[1]):
            problems.append("annotation %s degenerate box" % a["id"])
            continue
        rid = "a%d" % a["id"]
        name = CATEGORIES[c]
        if name == "Headline":
            text_regions.append(common.region(rid, name, box, type="headline", bw_id=a.get("bw_id")))
        else:
            nontext.append(common.region(rid, name, box, type=name.lower(), bw_id=a.get("bw_id")))
    ident = parse_identity(img.get("url"))
    meta = {"ca_identity": ident, "ca_identity_parsed": ident is not None, "language": "en",
            "problems": problems, "source_url": img.get("url")}
    if ident:
        meta["year"] = ident["year"]
        meta["date"] = ident["date"]
        meta["lccn"] = ident["lccn"]
    return common.make_row(DATASET, split, str(img["id"]), image_path, w, h, LICENCE, True,
                           text_regions=text_regions, nontext_regions=nontext, meta=meta)


def parquet_counts(data_dir):
    import pyarrow.parquet as pq
    counts = {}
    for fn in sorted(os.listdir(data_dir)):
        if not fn.endswith(".parquet"):
            continue
        pf = pq.ParquetFile(os.path.join(data_dir, fn))
        for batch in pf.iter_batches(batch_size=64, columns=["image_id", "objects"]):
            for rec in batch.to_pylist():
                counts[int(rec["image_id"])] = len(rec["objects"] or [])
    return counts


def convert(data_dir, out_dir, check_parquet=True, verify_sizes=True):
    d = os.path.join(data_dir, "data")
    zpath = os.path.abspath(os.path.join(d, "images.zip"))
    pcount = parquet_counts(d) if check_parquet else None
    zf = zipfile.ZipFile(zpath) if verify_sizes else None
    summary = {}
    for split, fn in SPLITS.items():
        with open(os.path.join(d, fn)) as f:
            coco = json.load(f)
        by_img = {}
        for a in coco["annotations"]:
            by_img.setdefault(a["image_id"], []).append(a)

        def rows():
            for n, img in enumerate(sorted(coco["images"], key=lambda i: i["id"])):
                r = row_from_coco(img, by_img.get(img["id"], []), split,
                                  "%s!images/%s" % (zpath, img["file_name"]))
                if pcount is not None and pcount.get(img["id"]) != len(by_img.get(img["id"], [])):
                    r["meta"]["problems"].append("parquet object count %s != json %d" % (
                        pcount.get(img["id"]), len(by_img.get(img["id"], []))))
                if zf is not None:
                    try:
                        with zf.open("images/" + img["file_name"]) as fh:
                            sw, sh = common.image_size(fh.read())
                        if (sw, sh) != (r["width"], r["height"]):
                            r["meta"]["problems"].append("image size %dx%d != json %dx%d" % (
                                sw, sh, r["width"], r["height"]))
                    except KeyError:
                        r["meta"]["problems"].append("image missing from images.zip")
                yield r
                if (n + 1) % 500 == 0:
                    common.log("locbw %s: %d pages" % (split, n + 1))
        out = os.path.join(out_dir, "%s__%s.jsonl" % (DIRNAME, split))
        summary[split] = common.write_jsonl(out, rows())
        common.log("locbw %s: wrote %d rows" % (split, summary[split]))
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--no-check-parquet", action="store_true")
    ap.add_argument("--no-verify-sizes", action="store_true", help="skip reading image headers from images.zip")
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    print(convert(a.data_dir, a.out_dir, not a.no_check_parquet, not a.no_verify_sizes))
    return 0


if __name__ == "__main__":
    sys.exit(main())
