#!/usr/bin/env python3
"""Per page: share of reference body-text regions whose majority predicted class is cartoon_or_advertisement.

Descriptive aside to diag.py on the same holdout pages (trained model at 0.25; American Stories at 0.01).

    adshare.py --prep DIR --root DIR
"""
import argparse
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import diag  # noqa: E402

BODY = {"ARTICLE-TEXT", "TEXT"}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--prep", required=True)
    ap.add_argument("--root", required=True)
    a = ap.parse_args(argv)
    out = {}
    for s in diag.SETS:
        regions = json.load(open(os.path.join(a.prep, "regions-%s.json" % s)))
        tr = json.load(open(os.path.join(a.root, "out", "trained-boxes-%s.json" % s)))
        asb = json.load(open(os.path.join(a.root, "out", "as-boxes-%s.json" % s)))
        for det, bx, conf in (("trained@0.25", tr["pages"], 0.25), ("american_stories@0.01", asb, 0.01)):
            per = []
            for pid in sorted(regions):
                size = (regions[pid]["width"], regions[pid]["height"])
                pm, sc = diag.paint_pred(bx[pid], conf, size)
                pm = np.where(pm < 0, 99, pm)
                n = ad = 0
                for r in regions[pid]["regions"]:
                    if r[4] not in BODY:
                        continue
                    x0, x1 = int(max(0, r[0] * sc)), int(min(diag.GRID, np.ceil(r[2] * sc)))
                    y0, y1 = int(max(0, r[1] * sc)), int(min(pm.shape[0], np.ceil(r[3] * sc)))
                    cell = pm[y0:y1, x0:x1].ravel()
                    if cell.size == 0:
                        continue
                    n += 1
                    ad += int(np.bincount(cell).argmax() == diag.NAMES.index("cartoon_or_advertisement"))
                per.append((pid, n, ad))
            tot_n, tot_ad = sum(p[1] for p in per), sum(p[2] for p in per)
            sh = sorted((p[2] / p[1] if p[1] else 0.0) for p in per)
            ranked = sorted(per, key=lambda p: -p[2])
            top10 = sum(p[2] for p in ranked[:max(1, len(per) // 10)])
            out["%s %s" % (s, det)] = {
                "pages": len(per), "body_regions": tot_n, "ad_majority_regions": tot_ad,
                "share": round(tot_ad / tot_n, 4),
                "page_share_quantiles_10_50_90_100": [round(sh[int(q * (len(sh) - 1))], 3) for q in (0.1, 0.5, 0.9, 1.0)],
                "pages_with_no_ad_region": sum(1 for p in per if p[2] == 0),
                "share_of_ad_regions_on_top_10pct_pages": round(top10 / tot_ad, 3) if tot_ad else 0.0}
    print(json.dumps(out, indent=1, sort_keys=True))
    json.dump(out, open(os.path.join(a.root, "results", "adshare.json"), "w"), indent=1, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
