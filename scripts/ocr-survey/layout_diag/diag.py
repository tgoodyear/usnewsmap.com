#!/usr/bin/env python3
"""Descriptive column-gutter and class-confusion diagnostics on the #2107 early-stopping holdouts.

Not a test. Inputs are the holdout pages only (finlam 62, La Liberte 200; Teklia, MIT): the trained model
used them for checkpoint selection, American Stories (Dell et al., arXiv:2308.12477) never saw them, and
the banded finder's tuning saw all 62 finlam pages and 9 of the 200 La Liberte pages. Metric: the step 1
primary metric (layout_columns/score.py). Method for the finder follows jscancella/NYTribuneOCRExperiments
(findText_usingSums.py, GPL-3.0, reimplemented); detector: Ultralytics YOLOv8 (AGPL-3.0).

    diag.py --prep DIR --out DIR [--workers 4]

Reads prep/{refs,regions}-<set>.json, cache/<set>, out/as-boxes-<set>.json, out/trained-boxes-<set>.json.
"""
import argparse
import json
import os
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
sys.path.insert(0, os.path.join(HERE, "..", "layout_train"))
import columns  # noqa: E402
import refgutters  # noqa: E402
import refs as refsmod  # noqa: E402
import score  # noqa: E402
import classmap  # noqa: E402
import asmodel  # noqa: E402

NAMES = classmap.NAMES
SETS = ("finlam", "liberte")
ARTICLE = {"article"}
TEXTLIKE = {"article", "headline", "author", "image_caption"}
OBSTACLE = {"cartoon_or_advertisement", "photograph", "table", "masthead", "newspaper_header"}
MARGIN = 0.01


def gutters_generic(boxes, size, conf, text, obstacles):
    """asmodel.gutters_from_boxes with configurable classes. boxes: [x0, y0, x1, y1, conf, class name]."""
    w, h = size
    body = [tuple(b[:4]) for b in boxes if b[4] >= conf and b[5] in text and b[2] > b[0] and b[3] > b[1]]
    non = [list(b[:4]) for b in boxes if b[4] >= conf and b[5] in obstacles]
    chains = refgutters.merge_gutters(refgutters.pair_gutters(body), w, h) if len(body) > 1 else []
    chains = refsmod.cap_gutters(chains, w)
    return [[[round(v) for v in s] for s in g] for g in chains if score.excluded_fraction(g, non) <= score.IGNORE_FRAC]


def subtract(y0, y1, ivs):
    """[(a, b)] pieces of [y0, y1] not covered by the intervals ivs."""
    out, cur = [], y0
    for a, b in sorted(ivs):
        if b <= cur:
            continue
        if a > cur:
            out.append((cur, min(a, y1)))
        cur = max(cur, b)
        if cur >= y1:
            break
    if cur < y1:
        out.append((cur, y1))
    return [(a, b) for a, b in out if b > a]


def clip_by_text(gutters, boxes, size, conf):
    """Banded gutters kept only where no detector text box crosses them (box spans the gutter's x centre
    with at least 1% of page width on each side), as segments with the crossed y ranges cut out."""
    m = MARGIN * size[0]
    tb = [b for b in boxes if b[4] >= conf and b[5] in TEXTLIKE]
    out = []
    for g in gutters:
        kept = []
        for x0, x1, y0, y1 in g:
            xc = (x0 + x1) / 2
            ivs = [(b[1], b[3]) for b in tb if b[0] < xc - m and b[2] > xc + m and b[3] > y0 and b[1] < y1]
            kept += [[x0, x1, a, b] for a, b in subtract(y0, y1, ivs)]
        if kept:
            out.append(kept)
    return out


def add_where_none(base_gutters, det_gutters, size):
    """Detector gutters, with the y ranges where a banded gutter already sits at the same x removed."""
    m = MARGIN * size[0]
    segs = [s for g in base_gutters for s in g]
    out = []
    for g in det_gutters:
        kept = []
        for x0, x1, y0, y1 in g:
            ivs = [(s[2], s[3]) for s in segs if x0 - m <= (s[0] + s[1]) / 2 <= x1 + m and s[3] > y0 and s[2] < y1]
            kept += [[x0, x1, a, b] for a, b in subtract(y0, y1, ivs)]
        if kept:
            out.append(kept)
    return out


def agnostic_nms(boxes, conf, iou=asmodel.IOU, max_det=asmodel.MAX_DET):
    """American Stories' class-agnostic NMS (IoU 0.1, 2,000 boxes) applied to a detector's boxes at >= conf."""
    keep = [b for b in boxes if b[4] >= conf]
    if len(keep) < 2:
        return keep
    arr = np.array([b[:4] for b in keep], dtype=np.float64)
    idx = asmodel.nms_agnostic(arr, np.array([b[4] for b in keep]), iou, max_det)
    return [keep[i] for i in idx]


def configs(banded, boxes_as, boxes_tr, sizes):
    """name -> {page id: gutters}. Trained-model rows come twice: its own per-class NMS (IoU 0.7), and with
    American Stories' agnostic NMS (IoU 0.1) applied first (TRn), the same box set the AS contender used."""
    out = {"1 banded": banded}
    plan = [("AS", boxes_as, 0.01), ("AS", boxes_as, 0.25), ("TR", boxes_tr, 0.25), ("TR", boxes_tr, 0.01)]
    for c in (0.25, 0.01):
        plan.append(("TRn", {p: agnostic_nms(b, c) for p, b in boxes_tr.items()}, c))
    for d, b, c in plan:
        art = {p: gutters_generic(b[p], sizes[p], c, ARTICLE, OBSTACLE) for p in b}
        txt = {p: gutters_generic(b[p], sizes[p], c, TEXTLIKE, OBSTACLE) for p in b}
        a = {p: clip_by_text(banded[p], b[p], sizes[p], c) for p in b}
        tag = "%s@%.2f" % (d, c)
        out["2 %s article-only" % tag] = art
        out["3 %s text-like" % tag] = txt
        out["4a banded minus %s crossings" % tag] = a
        out["4b (a) + %s article gutters where banded none" % tag] = {p: a[p] + add_where_none(banded[p], art[p], sizes[p]) for p in b}
        out["4b' (a) + %s text-like gutters where banded none" % tag] = {p: a[p] + add_where_none(banded[p], txt[p], sizes[p]) for p in b}
    return out


# ------------------------------------------------------------------ class confusion
GRID = 400
NREF = len(NAMES) + 2  # classes, "other" (a dataset class with no mapping), "background" (no reference region)
NPRED = len(NAMES) + 1  # classes, "none"


def paint_pred(boxes, conf, size):
    sc = GRID / size[0]
    H = max(1, round(size[1] * sc))
    pm = np.full((H, GRID), -1, dtype=np.int8)
    for b in sorted((b for b in boxes if b[4] >= conf), key=lambda b: -b[4]):
        x0, x1 = int(max(0, b[0] * sc)), int(min(GRID, np.ceil(b[2] * sc)))
        y0, y1 = int(max(0, b[1] * sc)), int(min(H, np.ceil(b[3] * sc)))
        if x1 > x0 and y1 > y0:
            sub = pm[y0:y1, x0:x1]
            sub[sub < 0] = NAMES.index(b[5])
    return pm, sc


def paint_ref(regions, size):
    sc = GRID / size[0]
    H = max(1, round(size[1] * sc))
    rm = np.full((H, GRID), NREF - 1, dtype=np.int8)
    items = []
    for r in regions:
        m = r[5]
        items.append((r, NAMES.index(m) if m in NAMES else NREF - 2))
    for r, k in sorted(items, key=lambda t: -((t[0][2] - t[0][0]) * (t[0][3] - t[0][1]))):
        x0, x1 = int(max(0, r[0] * sc)), int(min(GRID, np.ceil(r[2] * sc)))
        y0, y1 = int(max(0, r[1] * sc)), int(min(H, np.ceil(r[3] * sc)))
        if x1 > x0 and y1 > y0:
            rm[y0:y1, x0:x1] = k
    return rm, items, sc


def confusion(regions, boxes, sizes, conf):
    """Pixel matrix (reference class x predicted class, original-page pixels) and region-level counts."""
    pix = np.zeros((NREF, NPRED))
    reg, bysrc = {}, {}
    for pid in sorted(regions):
        size = sizes[pid]
        rm, items, sc = paint_ref(regions[pid]["regions"], size)
        pm, _ = paint_pred(boxes[pid], conf, size)
        pm = np.where(pm < 0, NPRED - 1, pm).astype(int)
        w = 1.0 / (sc * sc)
        np.add.at(pix, (rm.astype(int).ravel(), pm.ravel()), w)
        for r, k in items:
            if k >= len(NAMES):
                continue
            x0, x1 = int(max(0, r[0] * sc)), int(min(GRID, np.ceil(r[2] * sc)))
            y0, y1 = int(max(0, r[1] * sc)), int(min(pm.shape[0], np.ceil(r[3] * sc)))
            cell = pm[y0:y1, x0:x1].ravel()
            if cell.size == 0:
                continue
            cnt = np.bincount(cell, minlength=NPRED)
            j = int(cnt.argmax())
            key = (NAMES[k], (NAMES + ["none"])[j] if cnt[j] / cell.size >= 0.5 else "mixed (no class has half)")
            reg[key] = reg.get(key, 0) + 1
            sk = (r[4], key[1])
            bysrc[sk] = bysrc.get(sk, 0) + 1
    return pix, reg, bysrc


def fmt_conf(pix):
    cols = NAMES + ["none"]
    rows = NAMES + ["other (unmapped)", "background"]
    out = {}
    for i, rn in enumerate(rows):
        tot = pix[i].sum()
        if tot <= 0:
            continue
        out[rn] = {"area_px": round(float(tot)), "share": {c: round(float(pix[i, j] / tot), 4) for j, c in enumerate(cols)}}
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--prep", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--root", required=True, help="dir holding cache/ and out/")
    a = ap.parse_args(argv)
    os.makedirs(a.out, exist_ok=True)
    res = {"sets": {}, "confusion": {}}
    sets_cfg = {}
    for s in SETS:
        refs = json.load(open(os.path.join(a.prep, "refs-%s.json" % s)))
        pids = sorted(refs["pages"])
        sizes = {p: (refs["pages"][p]["width"], refs["pages"][p]["height"]) for p in pids}
        b_as = json.load(open(os.path.join(a.root, "out", "as-boxes-%s.json" % s)))
        tr = json.load(open(os.path.join(a.root, "out", "trained-boxes-%s.json" % s)))
        b_tr = tr["pages"]
        assert sorted(b_as) == pids == sorted(b_tr), "page sets differ"
        for p in pids:
            assert list(sizes[p]) == tr["sizes"][p], "size mismatch " + p
        p0 = columns.Params()
        banded = {}
        t0 = time.time()
        for n, p in enumerate(pids, 1):
            gray, size = columns.load_cached(os.path.join(a.root, "cache", s), p)
            banded[p] = columns.detect(gray, size, "banded", p0)
            if n % 50 == 0:
                print("%s banded %d/%d %.0fs" % (s, n, len(pids), time.time() - t0), flush=True)
        cfgs = configs(banded, b_as, b_tr, sizes)
        sets_cfg[s] = (refs, cfgs)
        regions = json.load(open(os.path.join(a.prep, "regions-%s.json" % s)))
        res["confusion"][s] = {}
        for det, bx in (("trained", b_tr), ("american_stories", b_as)):
            for conf in (0.25, 0.01):
                pix, reg, bysrc = confusion(regions, bx, sizes, conf)
                res["confusion"][s]["%s@%.2f" % (det, conf)] = {
                    "pixels": fmt_conf(pix), "regions_majority": {"%s -> %s" % k: v for k, v in sorted(reg.items())},
                    "regions_by_source_class": {"%s -> %s" % k: v for k, v in sorted(bysrc.items())}, "pix_raw": pix.tolist()}
        print(s, "configs", len(cfgs), flush=True)
    names = list(sets_cfg["finlam"][1])
    table = {}
    for name in names:
        sets = {s + "-holdout": {"detections": {p: {"gutters": g, "height": sets_cfg[s][0]["pages"][p]["height"],
                                                    "width": sets_cfg[s][0]["pages"][p]["width"]} for p, g in sets_cfg[s][1][name].items()},
                                 "refs": sets_cfg[s][0]["pages"], "for_publication": True} for s in SETS}
        r = score.score_sets(sets, per_page=False)
        table[name] = {"pool": {"p": r["precision"], "r": r["recall"], "f1": r["f1"], "f1_ci": r["interval_95"]},
                       "sets": {k: {"p": v["precision"], "r": v["recall"], "f1": v["f1"], "f1_ci": v["interval_95"]["f1"] if isinstance(v["interval_95"], dict) else v["interval_95"],
                                    "unscored_c": v["unscored_share_c"],
                                    "det_height": v["totals"]["det_height"]} for k, v in r["per_set"].items()}}
        print("%-60s pool P %.3f R %.3f F1 %.3f | %s" % (name, r["precision"], r["recall"], r["f1"],
              " | ".join("%s %.3f/%.3f/%.3f" % (k[:3], v["precision"], v["recall"], v["f1"]) for k, v in r["per_set"].items())), flush=True)
    res["columns"] = table
    json.dump(res, open(os.path.join(a.out, "diag-results.json"), "w"), indent=1, sort_keys=True)
    json.dump({s: sets_cfg[s][1] for s in SETS}, open(os.path.join(a.out, "diag-gutters.json"), "w"), sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
