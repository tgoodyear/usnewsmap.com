#!/usr/bin/env python3
"""English rules 1 and 3 of the combined segmenter's frozen plan (#2107, comment 7131927738), on Beyond Words pages.

Both use banded v2's false-gutter measure (layout_diag/bwfalse.py): Beyond Words' marked boxes (photograph,
illustration, comics/cartoon, editorial cartoon, map, advertisement, and headlines at least 25% of the page width
wide), scaled from Beyond Words' image to the scan; a gutter piece is the vertical line at its x centre over its
own y range, and counts as inside when at least half its length lies inside the union of the boxes.
Per page and method: false height (pieces inside), all height, and outside height (all minus false).
  Rule 1: the combiner's false-gutter share (pooled false / pooled all) is below banded v1's, the paired
          difference's 95% interval below 0.
  Rule 3: the combiner must not pass by deleting gutters: its pooled outside height is at least 85% of banded
          v1's on the same pages (ratio >= 0.85), with the paired interval's lower bound above 0.80.
Paired page bootstrap: 1,000 draws, seed 2107, the same draws for both methods, 2.5 and 97.5 percentiles.
Pages are those in both detection files and in --scan-sizes (bw_train.py's finish output shape); a page left out
upstream (no scan, aspect) is never here. Validation rows (the English test) are refused without --measure-run.

    english_rules.py --rows BW.jsonl --scan-sizes scan-sizes.json --combiner det.json --banded v1.json --out F
                     [--measure-run]
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
for d in (os.path.join(HERE, "..", "layout_columns"), os.path.join(HERE, "..", "layout_diag")):
    sys.path.insert(0, d)
import bwfalse  # noqa: E402
import score  # noqa: E402

RULE3_RATIO, RULE3_LOWER = 0.85, 0.80


def scaled_boxes(row, size):
    sx, sy = size[0] / row["width"], size[1] / row["height"]
    return [[b[0] * sx, b[1] * sy, b[2] * sx, b[3] * sy] for b in bwfalse.marked_boxes(row)]


def lines_of(gutters):
    """Vertical lines at each piece's x centre (bwfalse's reading of a non-v2 method)."""
    return [[{"x_top": (s[0] + s[1]) / 2.0, "x_bottom": (s[0] + s[1]) / 2.0, "y0": float(s[2]), "y1": float(s[3])}
             for s in g] for g in gutters]


def page_vec(gutters, boxes):
    f, t = bwfalse.page_false(lines_of(gutters), boxes)
    return [f, t, t - f]


def pooled(v):
    s = v.sum(axis=0)
    share = s[0] / s[1] if s[1] > 0 else 0.0
    return share, s[2]


def rules(vc, vb):
    """vc, vb: (pages, 3) arrays [false, all, outside] for the combiner and banded v1."""
    sc, oc = pooled(vc)
    sb, ob = pooled(vb)
    ratio = oc / ob if ob > 0 else float("nan")
    rng = np.random.default_rng(score.SEED)
    n = len(vc)
    d_share, d_ratio = [], []
    for _ in range(score.DRAWS):
        i = rng.integers(0, n, n)
        a, b = pooled(vc[i]), pooled(vb[i])
        d_share.append(a[0] - b[0])
        d_ratio.append(a[1] / b[1] if b[1] > 0 else np.nan)
    ci = lambda x: [float(v) for v in np.nanpercentile(np.array(x, dtype=float), [2.5, 97.5])]  # noqa: E731
    r1, r3 = ci(d_share), ci(d_ratio)
    return {"pages": n,
            "false_share": {"combiner": sc, "banded_v1": sb, "diff": sc - sb, "diff_interval_95": r1},
            "outside_height": {"combiner": float(oc), "banded_v1": float(ob), "ratio": ratio, "ratio_interval_95": r3},
            "rule1_pass": bool(r1[1] < 0),
            "rule3_pass": bool(ratio >= RULE3_RATIO and r3[0] > RULE3_LOWER)}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--rows", required=True)
    ap.add_argument("--scan-sizes", required=True)
    ap.add_argument("--combiner", required=True)
    ap.add_argument("--banded", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--measure-run", action="store_true")
    a = ap.parse_args(argv)
    rows = {str(r["page_id"]): r for r in bwfalse.read_rows(a.rows)}
    if any(r.get("split") == "val" for r in rows.values()) and not a.measure_run:
        raise SystemExit("validation rows are the English test: --measure-run only")
    sizes = json.load(open(a.scan_sizes))
    c = json.load(open(a.combiner))
    b = json.load(open(a.banded))
    c.pop("_meta", None)
    b.pop("_meta", None)
    ids = sorted(set(c) & set(b) & set(sizes) & set(rows))
    vc, vb = [], []
    for p in ids:
        boxes = scaled_boxes(rows[p], (sizes[p]["width"], sizes[p]["height"]))
        vc.append(page_vec(c[p]["gutters"], boxes))
        vb.append(page_vec(b[p]["gutters"], boxes))
    res = rules(np.array(vc, dtype=float).reshape(-1, 3), np.array(vb, dtype=float).reshape(-1, 3))
    res["settings"] = {"draws": score.DRAWS, "seed": score.SEED, "inside": bwfalse.INSIDE, "rule3_ratio": RULE3_RATIO,
                       "rule3_lower": RULE3_LOWER}
    with open(a.out, "w") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    print(json.dumps(res, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
