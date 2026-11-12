"""A check on a finder's boxes that needs no truth (#1622): how much of a page's ink they hold, and how
box-like the boxes are. A page with a poor layout should show here before anything is read.

  --found found.jsonl --scans DIR [--found-id NAME]   -> JSON Lines on standard output, one per page:
  {"page", "lines", "ink_coverage", "tall_share", "wide_share", "median_h"}

ink_coverage: the share of the page's ink (Sauvola, on the page reduced 4 times, specks dropped) inside the union
of the boxes. tall_share: boxes over 1.8 times the median height (merged lines). wide_share: boxes over half
the page's width (merged columns). The page's scan is read from DIR/<doc_id>.jpg or, with --ghn-images, from
that directory's `<lccn>/<date>/ed-N/seq-N.jpg`.
"""
import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

Image.MAX_IMAGE_PIXELS = None


def ink_mask(img, reduce=4):
    g = np.asarray(img.reduce(reduce), dtype=np.float32)
    mean = ndimage.uniform_filter(g, 15)
    sq = ndimage.uniform_filter(g * g, 15)
    std = np.sqrt(np.maximum(sq - mean * mean, 0))
    ink = g < mean * (1 + 0.3 * (std / 64 - 1))
    lab, n = ndimage.label(ink, structure=np.ones((3, 3)))
    sizes = ndimage.sum(ink, lab, range(1, n + 1))
    return np.isin(lab, 1 + np.flatnonzero(sizes >= 4))


def page_qa(img, lines, reduce=4):
    ink = ink_mask(img, reduce)
    cover = np.zeros(ink.shape, bool)
    hs, ws = [], []
    for l in lines:
        x0, y0, x1, y1 = l["box"]
        if x1 - x0 < 8 or y1 - y0 < 8:
            continue
        cover[y0 // reduce:y1 // reduce + 1, x0 // reduce:x1 // reduce + 1] = True
        hs.append(y1 - y0)
        ws.append(x1 - x0)
    med = float(np.median(hs)) if hs else 0.0
    return {"lines": len(hs), "ink_coverage": round(float((ink & cover).sum() / max(ink.sum(), 1)), 4),
            "tall_share": round(sum(h > 1.8 * med for h in hs) / max(len(hs), 1), 4),
            "wide_share": round(sum(w > 0.5 * img.width for w in ws) / max(len(ws), 1), 4), "median_h": med}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--found", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--ghn-images", action="store_true", help="--scans holds `<lccn>/<date>/ed-N/seq-N.jpg`")
    a = ap.parse_args()
    for r in map(json.loads, open(a.found)):
        path = Path(a.scans) / (f"{r['page']}.jpg" if a.ghn_images else f"{r['page'].replace('/', '_')}.jpg")
        print(json.dumps({"page": r["page"], **page_qa(Image.open(path).convert("L"), r["lines"])}))


if __name__ == "__main__":
    main()
