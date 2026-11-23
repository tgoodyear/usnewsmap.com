#!/usr/bin/env python3
"""S1 of #2128: columns first, with the #2107 banded column finder frozen at commit 337fc0aa
(`scripts/ocr-survey/layout_columns/columns.py`, default parameters, method `banded`), then S0 inside each
column.

The column finder gives gutters, each a list of segments [x0, x1, y0, y1] in scan pixels. A gutter becomes a
vertical cut at the median of its segments' centres, over its own height (top of its first segment to the
bottom of its last). The page is cut into horizontal bands at the cuts' ends; neighbouring bands with the
same cuts are merged; inside each band the cuts that cover it split it into columns. So a heading wider than
the columns, above where the gutters start, stays one region. Each region of the page's Sauvola-binarised
scan (S0's own `chr_layout_find.prepare`) is read with Tesseract PSM 3 exactly as S0 reads a page, its
boxes are shifted back into page pixels, and the page's boxes go through S0's clean-up together (with the
page's width). A region under 20 pixels either way is skipped. tid is "r<region>.<block>.<par>.<line>".

    s1_columns.py --pages pages.txt --scans DIR --layout ghn|loc --tessdata DIR --columns-py columns.py
                  --out found-s1.jsonl [--jobs N]

--columns-py is the frozen file, e.g. from `git show 337fc0aa:scripts/ocr-survey/layout_columns/columns.py`;
its sha256 is written to every record.
"""
import argparse
import concurrent.futures as cf
import importlib.util
import json
import os
import sys
import tempfile

import numpy as np

import common
import chr_layout_post as lp  # noqa: E402
import s0_find  # noqa: E402

MIN_REGION = 20


def load_columns(path):
    spec = importlib.util.spec_from_file_location("columns_frozen", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def regions(gutters, w, h):
    """Rectangles [x0, y0, x1, y1] tiling the page, from the gutters as described above."""
    cuts = []
    for g in gutters:
        xs = sorted((s[0] + s[1]) / 2 for s in g)
        cuts.append((float(np.median(xs)), min(s[2] for s in g), max(s[3] for s in g)))
    ys = sorted({0, h, *(max(0, min(h, c[1])) for c in cuts), *(max(0, min(h, c[2])) for c in cuts)})
    bands = []
    for y0, y1 in zip(ys, ys[1:]):
        if y1 <= y0:
            continue
        active = tuple(sorted(round(c[0]) for c in cuts if c[1] <= y0 and c[2] >= y1))
        if bands and bands[-1][2] == active and bands[-1][1] == y0:
            bands[-1] = (bands[-1][0], y1, active)
        else:
            bands.append((y0, y1, active))
    out = []
    for y0, y1, active in bands:
        xs = [0, *[x for x in active if 0 < x < w], w]
        for x0, x1 in zip(xs, xs[1:]):
            out.append([int(x0), int(y0), int(x1), int(y1)])
    return out


def do_page(args):
    page, scan, tessdata, columns_py = args
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    col = load_columns(columns_py)
    p = col.Params()
    gray, size = col.load_gray(scan, p.work_width)
    gutters = col.detect(gray, size, "banded", p)
    del gray
    w, h = size
    regs = regions(gutters, w, h)
    raw = []
    with tempfile.TemporaryDirectory() as tmp:
        prepped = s0_find.sauvola(scan, tmp)
        img = Image.open(prepped)
        img.load()
        for k, (x0, y0, x1, y1) in enumerate(regs):
            if x1 - x0 < MIN_REGION or y1 - y0 < MIN_REGION:
                continue
            path = f"{tmp}/region.png"
            img.crop((x0, y0, x1, y1)).save(path)
            for l in s0_find.tess_rows(path, tessdata, offset=(x0, y0)):
                raw.append({"box": l["box"], "tid": f"r{k}.{l['tid']}"})
    return {"page": page, "width": w, "height": h, "raw_lines": len(raw), "lines": lp.clean(raw, w, s0_find.MIN_H),
            "regions": regs, "gutters": gutters, "columns_py_sha256": common.sha256_file(columns_py)}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pages", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--layout", choices=["ghn", "loc"], required=True)
    ap.add_argument("--tessdata", required=True)
    ap.add_argument("--columns-py", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=1)
    ap.add_argument("--prep-slots", type=int, default=2)
    a = ap.parse_args()
    pages = common.read_pages(a.pages)
    done = {json.loads(l)["page"] for l in open(a.out)} if os.path.exists(a.out) else set()
    todo = [(p, str(common.scan_path(a.scans, p, a.layout)), a.tessdata, a.columns_py) for p in pages if p not in done]
    with open(a.out, "a") as out, s0_find.pool_of(a.jobs, a.prep_slots) as pool:
        for i, rec in enumerate(pool.map(do_page, todo), 1):
            out.write(json.dumps(rec, sort_keys=True) + "\n")
            out.flush()
            print(f"{i}/{len(todo)} {rec['page']} regions {len(rec['regions'])} lines {len(rec['lines'])}",
                  file=sys.stderr, flush=True)


if __name__ == "__main__":
    main()
