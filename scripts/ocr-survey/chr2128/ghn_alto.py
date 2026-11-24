#!/usr/bin/env python3
"""ALTO and page images for the GHN training pages, so `usnm-glyphs build` can cut glyphs from them (#2128).

GHN pages have no ALTO. For each training page with Cherokee lines, this stacks the page's Cherokee line
crops (the S0 boxes padded 12/8 px, as read) top to bottom, 40 px of white apart, in columns of at most
12,000 px, into one image, and writes
an ALTO file in pixels for it: one TextBlock, one TextLine per crop (the S0 box itself, inside the crop),
and one String per word `chr_phoenix` read, with the word box Tesseract gave (`ghn_words.py`). The
crate then treats these words exactly as LoC's: it keeps a word only when it is a known 1860 New
Testament form or one fix of the first type's misreadings turns into exactly one (usnm_core::cherokee_type),
and its `clean` keeps a glyph only when its neighbours on other pages agree.

The composite is saved as JPEG at quality 95, `<lccn>_<date>_ed-N_seq-N.jpg`, the name the crate reads, and
the ALTO as `<lccn>/<yyyy>/<mm>/<dd>/ed-N/seq-N/ocr.xml`. Only --pages are written (the 489 training pages;
no GHN test page ever gets an ALTO). A crop whose S0 box is over 1.8 times the page's median box height
(merged lines, the #1622 merged-lines test), or over 1.8 times the median of the pages' median heights (a
page whose boxes are mostly merged), is left out: its line band would hold several lines.

    ghn_alto.py --words words.jsonl --classified classified.jsonl --pages ghn-train-pages.txt
                --crops-dir DIR --out-alto DIR --out-images DIR
"""
import argparse
import collections
import json
from pathlib import Path
from xml.sax.saxutils import quoteattr

import numpy as np
from PIL import Image

import common

GAP = 40
MAX_H = 12000
MERGED = 1.8  # a box taller than this many page-median heights holds merged lines: no glyphs from it


def alto_path(root, page):
    lccn, date, ed, seq = page.split("/")
    y, m, d = date.split("-")
    return Path(root) / lccn / y / m / d / ed / seq / "ocr.xml"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--words", required=True)
    ap.add_argument("--classified", required=True)
    ap.add_argument("--pages", required=True)
    ap.add_argument("--crops-dir", required=True)
    ap.add_argument("--out-alto", required=True)
    ap.add_argument("--out-images", required=True)
    a = ap.parse_args()
    pages = set(common.read_pages(a.pages))
    boxes, med = {}, {}
    for rec in common.read_jsonl(a.classified):
        if rec["page"] in pages:
            hs = [l["box"][3] - l["box"][1] for l in rec["lines"] if l["box"][2] - l["box"][0] >= 8 and l["box"][3] - l["box"][1] >= 8]
            med[rec["page"]] = float(np.median(hs)) if hs else 0.0
            for l in rec["lines"]:
                boxes[common.line_id(rec["page"], l["tid"])] = l["box"]
    gmed = float(np.median([m for m in med.values() if m > 0]))  # the median page's median box height
    by_page = collections.defaultdict(list)
    for r in common.read_jsonl(a.words):
        if r["page"] in pages:
            by_page[r["page"]].append(r)
    Path(a.out_images).mkdir(parents=True, exist_ok=True)
    stats = {"pages": 0, "lines": 0, "words": 0, "skipped_merged_boxes": 0, "median_of_page_medians": gmed}
    for page, rows in sorted(by_page.items()):
        limit = min(MERGED * med[page], MERGED * gmed)
        keep = [r for r in rows if boxes[r["line_id"]][3] - boxes[r["line_id"]][1] <= limit]
        stats["skipped_merged_boxes"] += len(rows) - len(keep)
        rows = keep
        if not rows:
            continue
        rows.sort(key=lambda r: tuple(int(x) for x in r["line_id"].split("#")[1].split(".")))
        crops = [np.asarray(Image.open(Path(a.crops_dir) / r["crop"]).convert("L")) for r in rows]
        # stack top to bottom, starting a new column past MAX_H (a JPEG side is at most 65,535 px)
        places, x, y, colw, H = [], GAP, GAP, 0, 0
        for c in crops:
            if y > GAP and y + c.shape[0] + GAP > MAX_H:
                x, y, colw = x + colw + GAP, GAP, 0
            places.append((x, y))
            colw = max(colw, c.shape[1])
            y += c.shape[0] + GAP
            H = max(H, y)
        W = x + colw + GAP
        canvas = np.full((H, W), 255, np.uint8)
        xml_lines = []
        for k, (r, c, (ox, oy)) in enumerate(zip(rows, crops, places)):
            canvas[oy:oy + c.shape[0], ox:ox + c.shape[1]] = c
            px0, py0 = r["pixel_box"][0], r["pixel_box"][1]
            b = boxes[r["line_id"]]
            lx0, ly0, lx1, ly1 = ox + b[0] - px0, oy + b[1] - py0, ox + b[2] - px0, oy + b[3] - py0
            strings = []
            for j, w in enumerate(r["words"]):
                x0, _, x1, _ = w["box"]
                strings.append(f'<String ID="s{k}_{j}" CONTENT={quoteattr(w["text"])} HPOS="{ox + x0}" VPOS="{ly0}" '
                               f'WIDTH="{x1 - x0}" HEIGHT="{ly1 - ly0}"/>')
                stats["words"] += 1
            xml_lines.append(f'<TextLine ID="l{k}" HPOS="{lx0}" VPOS="{ly0}" WIDTH="{lx1 - lx0}" HEIGHT="{ly1 - ly0}">'
                             + "<SP/>".join(strings) + "</TextLine>")
        Image.fromarray(canvas).save(Path(a.out_images) / f"{common.doc_id(page)}.jpg", quality=95)
        xml = ('<?xml version="1.0" encoding="UTF-8"?>\n<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">'
               '<Description><MeasurementUnit>pixel</MeasurementUnit></Description>'
               f'<Layout><Page ID="p1" WIDTH="{W}" HEIGHT="{H}" PHYSICAL_IMG_NR="1"><PrintSpace>'
               '<TextBlock ID="b0" language="chr">' + "".join(xml_lines) + "</TextBlock></PrintSpace></Page></Layout></alto>\n")
        p = alto_path(a.out_alto, page)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(xml, encoding="utf-8")
        stats["pages"] += 1
        stats["lines"] += len(rows)
    print(json.dumps(stats))


if __name__ == "__main__":
    main()
