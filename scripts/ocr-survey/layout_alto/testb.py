#!/usr/bin/env python3
"""Test B of #2107 step 2: planted line merges, and the page-level signals over the joined Beyond Words pages.

Method sources: step 1 of #2107 (#2112; after jscancella/NYTribuneOCRExperiments findText_usingSums.py,
GPL-3.0, reimplemented); #2103; Beyond Words (Library of Congress, CC0, biglam/loc_beyond_words@6c7f5fb3);
American Stories (Dell et al., arXiv:2308.12477).

    testb.py --join-dir DIR --gutters banded.json --out testb.json [--as-gutters as.json] [--limit 300]

--join-dir is join_bw.py's output (join.json, alto/, cache/). --gutters is columns.py's output (banded finder,
frozen defaults) for the joined pages; --as-gutters is asmodel.py's, for the pages it covers.

Test B: the first --limit joined pages in sha256("2107:s2:" + page id) order. On each, every ALTO line is merged with
the line beside it across the nearest detected gutter (signals.plant_merge). One AUC pooled over pages: each page gives
its `cross_share` on the original (negative) and on the planted copy (positive). A page with no detected gutter, or
with no line beside a gutter (no merge possible), is skipped and counted. Must reach 0.90.

Also written (reported, not deciding): cross_share, missed_ink and block_split over every joined page with gutters,
their distribution, and the 20 pages with the highest cross_share.
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import altolayout  # noqa: E402
import columns  # noqa: E402
import signals  # noqa: E402
from join_bw import sha_key  # noqa: E402

AUC_BAR = 0.90


def load_page(join_dir, pid, gut):
    gray, size = columns.load_cached(os.path.join(join_dir, "cache"), pid)
    with open(os.path.join(join_dir, "alto", pid + ".xml"), "rb") as f:
        parsed = altolayout.parse(f.read(), size)
    return gray, size, parsed, gut["gutters"]


def test_b(join_dir, join, gutters, limit):
    sel = sorted((p for p, v in join["pages"].items() if v["status"] == "joined"), key=sha_key)[:limit]
    orig, planted, per, skipped = [], [], {}, {"no_gutter": 0, "no_line_beside_gutter": 0, "no_lines": 0}
    for n, pid in enumerate(sel, 1):
        gut = gutters.get(pid)
        if gut is None or not gut["gutters"]:
            skipped["no_gutter"] += 1
            per[pid] = {"skipped": "no_gutter"}
            continue
        _, size, parsed, g = load_page(join_dir, pid, gut)
        boxes = [ln["box"] for ln in parsed["lines"]]
        segs = signals.segments(g)
        before = signals.cross_share_of(boxes, segs, size[0])
        if before is None:
            skipped["no_lines"] += 1
            per[pid] = {"skipped": "no_lines"}
            continue
        new, merges = signals.plant_merge(boxes, g)
        if merges == 0:
            skipped["no_line_beside_gutter"] += 1
            per[pid] = {"skipped": "no_line_beside_gutter"}
            continue
        after = signals.cross_share_of(new, segs, size[0])
        orig.append(before)
        planted.append(after)
        per[pid] = {"cross_share": before, "cross_share_planted": after, "merges": merges}
        if n % 25 == 0:
            print(f"test B pages {n}/{len(sel)}", flush=True)
    y = np.array([0] * len(orig) + [1] * len(planted))
    s = np.array(orig + planted)
    return {"auc": signals.auc(y, s), "pages_selected": len(sel), "pages_scored": len(orig), "skipped": skipped,
            "bar": AUC_BAR, "passes": bool(len(orig) and signals.auc(y, s) is not None and signals.auc(y, s) >= AUC_BAR),
            "mean_cross_share_original": float(np.mean(orig)) if orig else None,
            "mean_cross_share_planted": float(np.mean(planted)) if planted else None,
            "pages_planted_higher": int(sum(b > a for a, b in zip(orig, planted))),
            "pages_equal": int(sum(b == a for a, b in zip(orig, planted))), "per_page": per}


def distributions(join_dir, join, gutters, ids, progress=True):
    from columns import Params, binarize
    p = Params()
    rows = {}
    for n, pid in enumerate(ids, 1):
        gut = gutters.get(pid)
        if gut is None:
            continue
        gray, size, parsed, g = load_page(join_dir, pid, gut)
        sig = signals.page_signals(parsed, g, size, binarize(gray, p))
        rows[pid] = {k: sig[k] for k in ("cross_share", "missed_ink", "block_split", "gutters", "lines")}
        rows[pid]["lccn"] = join["pages"][pid]["lccn"]
        rows[pid]["date"] = join["pages"][pid]["date"]
        if progress and n % 100 == 0:
            print(f"signal pages {n}/{len(ids)}", flush=True)
    out = {"pages": len(rows)}
    for k in ("cross_share", "missed_ink", "block_split"):
        v = np.array([r[k] for r in rows.values() if r[k] is not None], dtype=float)
        out[k] = {"n": int(len(v)), "none": int(len(rows) - len(v))}
        if len(v):
            out[k].update({"mean": float(v.mean()), "quantiles": {str(q): float(np.quantile(v, q)) for q in (0, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 1)}})
    top = sorted(((r["cross_share"], pid) for pid, r in rows.items() if r["cross_share"] is not None), key=lambda t: (-t[0], t[1]))[:20]
    out["top20_cross_share"] = [{"page_id": pid, "cross_share": cs, "lccn": rows[pid]["lccn"], "date": rows[pid]["date"]} for cs, pid in top]
    return out, rows


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--join-dir", required=True)
    ap.add_argument("--gutters", required=True)
    ap.add_argument("--as-gutters")
    ap.add_argument("--limit", type=int, default=300)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    join = json.load(open(os.path.join(a.join_dir, "join.json")))
    gutters = json.load(open(a.gutters))
    res = {"join_counts": join["_meta"]["counts"], "banded": {}, "american_stories": None}
    res["banded"]["test_b"] = test_b(a.join_dir, join, gutters, a.limit)
    ids = sorted(p for p, v in join["pages"].items() if v["status"] == "joined")
    res["banded"]["distributions_all_joined"], _ = distributions(a.join_dir, join, gutters, ids)
    if a.as_gutters:
        asg = json.load(open(a.as_gutters))
        sel = sorted((p for p, v in join["pages"].items() if v["status"] == "joined"), key=sha_key)[: a.limit]
        sel = [p for p in sel if p in asg]
        res["american_stories"] = {"test_b": test_b(a.join_dir, {"pages": {p: join["pages"][p] for p in sel}}, asg, a.limit),
                                   "distributions_selected": distributions(a.join_dir, join, asg, sel)[0]}
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True, indent=1)
    t = res["banded"]["test_b"]
    print(f"test B AUC {t['auc']} on {t['pages_scored']} pages, skipped {t['skipped']}, passes {t['passes']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
