#!/usr/bin/env python3
"""Turn the trained detector's boxes into column gutters for the step 1 scorer (one-time evaluation).

Same rule as the American Stories contender (layout_columns/asmodel.py gutters_from_boxes): gutters are
the gaps between side-by-side `article` boxes, capped at 3% of page width, and gutters mostly inside a
non-text box are ignored. Boxes below confidence 0.25 (the Ultralytics default) are dropped first; the
American Stories contender used all boxes at its own 0.01 floor.

Detector: Ultralytics YOLOv8 (AGPL-3.0) fine-tuned for #2107; American Stories, Dell et al., arXiv:2308.12477.

    eval_gutters.py --boxes shard0.json [shard1.json ...] --out detections.json [--conf 0.25]

Output is columns.py's format: {page id: {gutters, height, width}}, ready for score.py --sets-json.
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "layout_columns"))


def build(pages, sizes, conf):
    import asmodel
    res = {}
    for pid in sorted(pages):
        dets = [(d[0], d[1], d[2], d[3], d[4], asmodel.NAMES.index(d[5])) for d in pages[pid] if d[4] >= conf]
        w, h = sizes[pid]
        g = asmodel.gutters_from_boxes(dets, (w, h), (1.0, 1.0))
        res[pid] = {"gutters": [[[round(v) for v in s] for s in c] for c in g], "height": h, "width": w}
    return res


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--boxes", nargs="+", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--conf", type=float, default=0.25)
    a = ap.parse_args(argv)
    pages, sizes = {}, {}
    for p in a.boxes:
        d = json.load(open(p))
        pages.update(d["pages"])
        sizes.update(d["sizes"])
    res = build(pages, sizes, a.conf)
    res["_meta"] = {"method": "usnm-layout-2107-yolov8m", "conf": a.conf, "pages": len(pages)}
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
