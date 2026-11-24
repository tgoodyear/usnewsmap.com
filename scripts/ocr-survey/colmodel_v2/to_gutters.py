#!/usr/bin/env python3
"""Evaluation adapter: predicted column regions -> gutters scorable by layout_columns/score.py (#2107).

    to_gutters.py --preds preds.json --threshold 0.5 --out det.json [--contain 0.5]

Per page: keep boxes scoring at least --threshold; visit them by descending score and drop any box
with at least --contain of its area inside an already kept box (two detections of one column); then
gutters = colgeom.gutters_from_regions, i.e. the scorer's own reference rule (refgutters.py
pair_gutters + merge_gutters) applied to the kept regions. Output has the shape layout_columns'
columns.py writes and score.py reads: page_id -> {gutters: [[[x0, x1, y0, y1], ...]], height, width,
regions}, plus "_meta" (method "colv2", threshold, contain, model sha256 from the preds file). Use it
as one of score.py's --detections files, or in a --sets-json "detections" list.
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import colgeom  # noqa: E402


def _area(b):
    return max(0.0, b[2] - b[0]) * max(0.0, b[3] - b[1])


def _inter(a, b):
    return max(0.0, min(a[2], b[2]) - max(a[0], b[0])) * max(0.0, min(a[3], b[3]) - max(a[1], b[1]))


def page_regions(boxes, scores, threshold, contain=0.5):
    cand = sorted(((s, b) for b, s in zip(boxes, scores) if s >= threshold), key=lambda t: -t[0])
    kept = []
    for _, b in cand:
        a = _area(b)
        if a <= 0:
            continue
        if any(_inter(b, k) >= contain * a for k in kept):
            continue
        kept.append(list(b))
    return kept


def convert(preds, threshold, contain=0.5):
    out = {}
    for pid, p in sorted(preds.items()):
        if pid == "_meta":
            continue
        regs = page_regions(p["boxes"], p["scores"], threshold, contain)
        out[pid] = {"gutters": colgeom.gutters_from_regions(regs, p["width"], p["height"]),
                    "height": p["height"], "regions": regs, "width": p["width"]}
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--preds", required=True)
    ap.add_argument("--threshold", type=float, required=True)
    ap.add_argument("--contain", type=float, default=0.5)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    with open(a.preds) as f:
        preds = json.load(f)
    out = convert(preds, a.threshold, a.contain)
    m = preds.get("_meta", {})
    out["_meta"] = {"contain": a.contain, "method": "colv2", "model_sha256": m.get("model_sha256"),
                    "pages": len(out), "preds": os.path.basename(a.preds), "threshold": a.threshold}
    with open(a.out, "w") as f:
        json.dump(out, f, sort_keys=True)
    print(json.dumps(out["_meta"], sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
