#!/usr/bin/env python3
"""Europeana Newspapers ground truth -> common layout-reference rows (#2107).

Dataset: biglam/europeana-newspapers-ground-truth, CC0-1.0,
  https://huggingface.co/datasets/biglam/europeana-newspapers-ground-truth
  50 German pages (Berlin State Library, Fraktur, 18th-19th century), PAGE XML regions, 678
  SeparatorRegions, reading order, and the ABBYY FineReader ALTO output of the same pages.
  Source: Clemens Neudecker, "Historical Newspapers Ground Truth", Zenodo 2019,
  https://zenodo.org/records/2583866 (doi 10.5281/zenodo.2583866), produced in the EU project
  Europeana Newspapers.

What the Hugging Face copy keeps of the ALTO: the text flattened to plain text (`ocr_text`), not
word boxes. The row keeps it as meta.ocr_text next to meta.gt_text so a layout score can see real
OCR text, but it cannot be placed on the page.

Mapping:
  text_regions     TextRegion; type paragraph/heading/caption feed the gutters, header/page-number/
                   TOC-entry/drop-capital are kept but give no gutter.
  nontext_regions  ImageRegion and GraphicRegion (picture), TableRegion (table).
  separators       SeparatorRegion with orientation: vertical when the box is taller than wide.
                   Vertical ones are direct column-rule labels; meta.vertical_separators counts
                   them. Separator boxes are polygon bounding boxes and may be thin or tilted.
  gutters          refgutters.py chains over paragraph, heading and caption regions.
  column_gutters   the same over paragraph regions only.

    conv_europeana.py --data-dir DIR --out-dir DIR
"""
import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402

DATASET = "biglam/europeana-newspapers-ground-truth"
DIRNAME = "biglam__europeana-newspapers-ground-truth"
LICENCE = "CC0-1.0"
GUTTER_TYPES = {"paragraph", "heading", "caption"}
TEXT_TYPE = {"paragraph": "paragraph", "heading": "heading", "caption": "caption",
             "header": "header", "page-number": "page-number", "TOC-entry": "toc",
             "drop-capital": "drop-capital"}
NONTEXT_KIND = {"ImageRegion": "picture", "GraphicRegion": "picture", "TableRegion": "table"}


def row_from_record(rec, split, image_path, width, height):
    text_regions, nontext, seps, problems = [], [], [], []
    for r in rec["regions"]:
        poly = [[float(p[0]), float(p[1])] for p in (r.get("polygon") or [])]
        bb = r.get("bbox")
        if bb and len(bb) == 4:
            box = [float(v) for v in bb]
        elif len(poly) >= 3:
            box = common.box_of_polygon(poly)
        else:
            problems.append("region %s has no geometry" % r["id"])
            continue
        # bbox is [x0, y0, x1, y1] in the source; normalise if it arrives as x, y, w, h
        if box[2] <= box[0] or box[3] <= box[1]:
            if poly:
                box = common.box_of_polygon(poly)
            else:
                problems.append("region %s degenerate bbox" % r["id"])
                continue
        box = common.clamp_box(box, width, height)
        kind, typ = r["kind"], r.get("type")
        poly = poly if len(poly) >= 3 else None
        if kind == "TextRegion":
            text_regions.append(common.region(r["id"], typ or "none", box, poly,
                                              type=TEXT_TYPE.get(typ, "other"),
                                              n_chars=len(r.get("text") or "")))
        elif kind in NONTEXT_KIND:
            nontext.append(common.region(r["id"], kind, box, poly, type=NONTEXT_KIND[kind]))
        elif kind == "SeparatorRegion":
            w, h = box[2] - box[0], box[3] - box[1]
            seps.append(common.region(r["id"], "separator", box, poly,
                                      orientation="vertical" if h >= w else "horizontal"))
        else:
            problems.append("region %s unknown kind %r" % (r["id"], kind))
    ids = {r["id"] for r in text_regions + nontext + seps}
    order = [i for i in (rec.get("reading_order") or []) if i in ids]
    missing = len(rec.get("reading_order") or []) - len(order)
    if missing:
        problems.append("%d reading_order ids not in regions" % missing)
    for pos, rid in enumerate(order):
        for r in text_regions:
            if r["id"] == rid:
                r["order"] = pos
    gt = [r["box"] for r in text_regions if r["type"] in GUTTER_TYPES]
    gc = [r["box"] for r in text_regions if r["type"] == "paragraph"]
    meta = {
        "gt_text": rec.get("text"),
        "image_filename": rec.get("image_filename"),
        "language": "de",
        "ocr_text": rec.get("ocr_text"),
        "ocr_text_note": "ABBYY ALTO flattened to plain text in the source; no word boxes",
        "problems": problems,
        "vertical_separators": sum(1 for s in seps if s["orientation"] == "vertical"),
    }
    return common.make_row(
        DATASET, split, rec["page_id"], image_path, width, height, LICENCE, True,
        text_regions=text_regions, nontext_regions=nontext, separators=seps, reading_order=order,
        gutters=common.derive_gutters(gt, width, height),
        column_gutters=common.derive_gutters(gc, width, height), meta=meta)


def convert(data_dir, out_dir, limit=None):
    import pyarrow.parquet as pq
    summary = {}
    for fn in sorted(os.listdir(os.path.join(data_dir, "data"))):
        if not fn.endswith(".parquet"):
            continue
        split = fn.split("-")[0]
        path = os.path.abspath(os.path.join(data_dir, "data", fn))
        pf = pq.ParquetFile(path)
        cols = ["image", "page_id", "image_filename", "width", "height", "text", "ocr_text",
                "reading_order", "regions"]

        def rows():
            n = 0
            for batch in pf.iter_batches(batch_size=2, columns=cols):
                for rec in batch.to_pylist():
                    img = rec.pop("image")
                    w, h = common.image_size(img["bytes"])
                    if (w, h) != (rec["width"], rec["height"]):
                        common.log("size mismatch %s: image %s vs row %s" % (
                            rec["page_id"], (w, h), (rec["width"], rec["height"])))
                    yield row_from_record(rec, split, "%s#row=%d" % (path, n), w, h)
                    n += 1
                    if limit and n >= limit:
                        return
        out = os.path.join(out_dir, "%s__%s.jsonl" % (DIRNAME, split))
        summary[split] = common.write_jsonl(out, rows())
        common.log("europeana %s: wrote %d rows to %s" % (split, summary[split], out))
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--limit", type=int)
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    print(convert(a.data_dir, a.out_dir, a.limit))
    return 0


if __name__ == "__main__":
    sys.exit(main())
