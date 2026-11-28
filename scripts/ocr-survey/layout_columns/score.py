#!/usr/bin/env python3
"""Score detected column gutters against reference gutters (#2107, step 1).

Matching, as frozen in the plan:
  - A detected gutter matches a reference gutter if, over at least half the reference gutter's
    height (y axis), it lies within the reference gap widened by 1% of the page's own width on
    each side. A gutter is a list of segments [x0, x1, y0, y1] (its x range may drift down a
    skewed page); the covered height sums the y overlap of detected and reference segments whose
    x range is inside the widened gap.
  - Greedy one-to-one, largest vertical overlap first. A reference gutter takes at most one
    detected gutter; extra detected gutters in the same gap stay unmatched.
  - A detected gutter mostly (more than half its area) inside an excluded region (ad, picture,
    table, masthead, header) is ignored: it counts neither for nor against.
  - Precision = matched detected height / all (non-ignored) detected height. Recall = matched
    reference height / all reference height. Pooled over pages, in pixels (a secondary figure
    pools heights as fractions of page height). F1 = 2PR/(P+R) from the pooled values.
  - Intervals: page bootstrap, 1,000 draws, seed 2107, 2.5 and 97.5 percentiles.
  - Second amendment (primary precision): each detected gutter's height splits into (a) inside a
    reference gap, (b) inside a body-text zone shrunk horizontally by 1% of page width on each side
    ((a) wins an overlap) and (c) elsewhere, which is unscored; precision = a / (a + b), recall is the
    coverage recall below. Coverage precision, the (c) share and the one-to-one figures are secondary.
  - Amended primary metric (second attempt): height coverage instead of one-to-one. Recall is the
    share of reference gutter height lying within some detected gutter, precision the share of
    detected gutter height lying within some reference gap widened by 1% of page width (x only;
    heights on the y axis; overlapping pieces of one gap count once). One-to-one P/R/F1 stays as a
    secondary output ("one_to_one").
  - Second attempt (--sets-json): precision and recall are computed per set from that set's pooled
    heights, the pool's are the means over sets (equal weight), F1 = 2PR/(P+R) from the means; the
    bootstrap resamples pages within each set and recomputes the pool. The reference side (3% gap cap,
    Europeana separators, non-text masks) is built by refs.py. Sets whose refs file says
    for_publication false (Colorado) get no per-page numbers in the output.

Method follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied.

Datasets: Reichsanzeiger-GT, CC BY 4.0, Mannheim University Library; American Stories gold,
Apache-2.0 text, Dell Research Harvard, arXiv:2308.12477; Chronicling Germany, CC BY-NC 4.0,
Schultze et al. 2025, University of Bonn.

    score.py --detections a.json [b.json ...] --regions regions.jsonl --selection selection.tsv
             --out score.json [--only-ca-page] [--per-page]

Output is keyed JSON, keys sorted.
"""
import argparse
import json
import sys

import numpy as np

import refgutters

MARGIN = 0.01        # of page width
MIN_REF_COVER = 0.5  # of the reference gutter's height
IGNORE_FRAC = 0.5    # of a detected gutter's area inside excluded regions
DRAWS = 1000
SEED = 2107


def _overlap(a0, a1, b0, b1):
    return max(0.0, min(a1, b1) - max(a0, b0))


def height(g):
    """Height of a gutter: the sum of its segments' (x0, x1, y0, y1) heights."""
    return sum(s[3] - s[2] for s in g)


def _excluded_area(seg, boxes):
    """Area of segment (x0, x1, y0, y1) covered by the union of boxes (x0, y0, x1, y1)."""
    x0, x1, y0, y1 = seg
    clipped = []
    for b in boxes:
        bx0, bx1 = max(x0, b[0]), min(x1, b[2])
        by0, by1 = max(y0, b[1]), min(y1, b[3])
        if bx1 > bx0 and by1 > by0:
            clipped.append((bx0, bx1, by0, by1))
    if not clipped:
        return 0.0
    xs = sorted({c[0] for c in clipped} | {c[1] for c in clipped})
    cov = 0.0
    for xa, xb in zip(xs, xs[1:]):
        iv = sorted((c[2], c[3]) for c in clipped if c[0] <= xa and c[1] >= xb)
        tot, cur0, cur1 = 0.0, None, None
        for a, b in iv:
            if cur1 is None or a > cur1:
                if cur1 is not None:
                    tot += cur1 - cur0
                cur0, cur1 = a, b
            else:
                cur1 = max(cur1, b)
        if cur1 is not None:
            tot += cur1 - cur0
        cov += tot * (xb - xa)
    return cov


def excluded_fraction(g, boxes):
    """Fraction of gutter g's area (all segments) inside the union of the excluded boxes."""
    area = sum((s[1] - s[0]) * (s[3] - s[2]) for s in g)
    if area <= 0 or not boxes:
        return 0.0
    return sum(_excluded_area(s, boxes) for s in g) / area


def covered_height(d, r, m):
    """Height over which detected gutter d lies within reference gutter r's gap widened by m.

    Both are lists of segments (x0, x1, y0, y1); a gutter's x range may drift down the page, so the
    test is made segment against segment, on their shared y range.
    """
    tot = 0.0
    for sd in d:
        for sr in r:
            if sd[0] >= sr[0] - m and sd[1] <= sr[1] + m:
                tot += _overlap(sd[2], sd[3], sr[2], sr[3])
    return tot


def height_union(segs):
    """Height (y axis) of a set of pieces (x0, x1, y0, y1), each gap counted once.

    At every y, pieces whose x ranges overlap belong to the same gap and count once; pieces at
    different x (different gutters) each count. So overlapping pieces of one gutter add no height.
    """
    if not segs:
        return 0.0
    a = np.asarray(segs, dtype=float).reshape(-1, 4)
    ys = np.unique(np.concatenate([a[:, 2], a[:, 3]]))
    total = 0.0
    for lo, hi in zip(ys[:-1], ys[1:]):
        act = a[(a[:, 2] <= lo) & (a[:, 3] >= hi)]
        if len(act) == 0:
            continue
        groups, cur = 0, -np.inf
        for x0, x1 in act[np.argsort(act[:, 0]), :2]:
            if x0 > cur:
                groups += 1
                cur = x1
            else:
                cur = max(cur, x1)
        total += groups * (hi - lo)
    return total


def coverage_pieces(a_segs, b_segs, m):
    """Parts of the a pieces lying inside some b piece widened by m (x only), as pieces.

    A part is the y overlap of an a piece with a b piece whose widened x range contains the a
    piece's x range. Returns pieces (x0, x1, y0, y1) of the a side.
    """
    if not a_segs or not b_segs:
        return []
    a = np.asarray(a_segs, dtype=float).reshape(-1, 4)
    b = np.asarray(b_segs, dtype=float).reshape(-1, 4)
    inside = (a[:, None, 0] >= b[None, :, 0] - m) & (a[:, None, 1] <= b[None, :, 1] + m)
    lo = np.maximum(a[:, None, 2], b[None, :, 2])
    hi = np.minimum(a[:, None, 3], b[None, :, 3])
    ok = inside & (hi > lo)
    ia, ib = np.nonzero(ok)
    return [(a[i, 0], a[i, 1], lo[i, j], hi[i, j]) for i, j in zip(ia, ib)]


_UNION_CACHE = {}


def _ref_union(ref):
    key = id(ref)
    hit = _UNION_CACHE.get(key)
    if hit is None or hit[0] is not ref:
        hit = (ref, height_union([s for g in ref for s in g]))
        _UNION_CACHE[key] = hit
    return hit[1]


def text_pieces(dsegs, zones, m):
    """Parts of detected pieces lying inside a body-text zone shrunk by m on each side (x only)."""
    if not dsegs or not zones:
        return []
    z = np.asarray(zones, dtype=float).reshape(-1, 4)  # x0, y0, x1, y1
    zx0, zx1 = z[:, 0] + m, z[:, 2] - m
    good = zx1 > zx0
    z, zx0, zx1 = z[good], zx0[good], zx1[good]
    if not len(z):
        return []
    a = np.asarray(dsegs, dtype=float).reshape(-1, 4)
    inside = (a[:, None, 0] >= zx0[None, :]) & (a[:, None, 1] <= zx1[None, :])
    lo = np.maximum(a[:, None, 2], z[None, :, 1])
    hi = np.minimum(a[:, None, 3], z[None, :, 3])
    ok = inside & (hi > lo)
    ia, iz = np.nonzero(ok)
    return [(a[i, 0], a[i, 1], lo[i, j], hi[i, j]) for i, j in zip(ia, iz)]


def coverage_page(kept, ref, width, zones=None):
    """Height-coverage sums for one page (the primary metric from the amendment).

    Recall: reference piece height lying inside some detected gutter, at the 1% tolerance (the
    detected piece's x range inside the reference piece widened by it, as in the matching rule).
    Precision: detected height lying inside some reference gap widened by the tolerance. Overlapping
    pieces of one gap count once (height_union).
    """
    m = MARGIN * width
    dsegs = [s for g in kept for s in g]
    rsegs = [s for g in ref for s in g]
    a_pieces = coverage_pieces(dsegs, rsegs, m)
    det_cov = height_union(a_pieces)
    # amended precision: (a) inside a reference gap, (b) inside a body-text zone shrunk by m, where
    # (a) wins an overlap, (c) the rest (unscored). b is what the zones add to the union with (a).
    b_pieces = text_pieces(dsegs, zones or [], m)
    b_only = height_union(a_pieces + b_pieces) - det_cov if b_pieces else 0.0
    # the reference side: the reference piece parts that have a detected piece inside their gap
    ref_cov = height_union([(r[0], r[1], r[2], r[3]) for r in _ref_parts(rsegs, dsegs, m)])
    return {"a_det": det_cov, "ab_det": det_cov + b_only, "b_det": b_only, "cov_det": det_cov,
            "cov_det_union": height_union(dsegs), "cov_ref": ref_cov, "cov_ref_union": _ref_union(ref)}


def _ref_parts(rsegs, dsegs, m):
    """Reference piece parts (x0, x1, y0, y1) for which some detected piece lies inside the gap."""
    if not rsegs or not dsegs:
        return []
    r = np.asarray(rsegs, dtype=float).reshape(-1, 4)
    d = np.asarray(dsegs, dtype=float).reshape(-1, 4)
    inside = (d[None, :, 0] >= r[:, None, 0] - m) & (d[None, :, 1] <= r[:, None, 1] + m)
    lo = np.maximum(r[:, None, 2], d[None, :, 2])
    hi = np.minimum(r[:, None, 3], d[None, :, 3])
    ok = inside & (hi > lo)
    ir, idd = np.nonzero(ok)
    return [(r[i, 0], r[i, 1], lo[i, j], hi[i, j]) for i, j in zip(ir, idd)]


def match_page(det, ref, width, excluded, zones=None):
    """Per-page sums. det and ref are lists of gutters (lists of segments)."""
    kept = [d for d in det if excluded_fraction(d, excluded) <= IGNORE_FRAC]
    m = MARGIN * width
    cands = []
    hull = lambda g: (min(s[0] for s in g), max(s[1] for s in g), min(s[2] for s in g), max(s[3] for s in g))  # noqa: E731
    dh = [hull(d) for d in kept]
    rhh = [(hull(r), height(r)) for r in ref]
    for di, d in enumerate(kept):
        for ri, r in enumerate(ref):
            (rx0, rx1, ry0, ry1), rh = rhh[ri]
            if rh <= 0:
                continue
            dx0, dx1, dy0, dy1 = dh[di]
            if dy1 < ry0 or dy0 > ry1 or dx1 < rx0 - m or dx0 > rx1 + m:
                continue  # cannot lie inside the gap anywhere (exact shortcut)
            cov = covered_height(d, r, m)
            if cov >= MIN_REF_COVER * rh:
                cands.append((cov, di, ri))
    cands.sort(key=lambda c: (-c[0], c[1], c[2]))
    ud, ur = set(), set()
    md = mr = 0.0
    for cov, di, ri in cands:
        if di in ud or ri in ur:
            continue
        ud.add(di)
        ur.add(ri)
        md += height(kept[di])
        mr += height(ref[ri])
    out = coverage_page(kept, ref, width, zones)
    out.update({
        "det_height": sum(height(d) for d in kept),
        "det_ignored": len(det) - len(kept),
        "det_n": len(kept),
        "matched_det_height": md,
        "matched_ref_height": mr,
        "matched_n": len(ud),
        "ref_height": sum(height(r) for r in ref),
        "ref_n": len(ref),
    })
    return out


def prf(md, dh, mr, rh):
    p = md / dh if dh > 0 else 0.0
    r = mr / rh if rh > 0 else 0.0
    f = 2 * p * r / (p + r) if p + r > 0 else 0.0
    return p, r, f


def pooled(rows, key_d="det_height", key_md="matched_det_height", key_r="ref_height", key_mr="matched_ref_height"):
    return prf(sum(r[key_md] for r in rows), sum(r[key_d] for r in rows),
               sum(r[key_mr] for r in rows), sum(r[key_r] for r in rows))


KEYS = {
    "primary": ("ab_det", "a_det", "cov_ref_union", "cov_ref"),
    "coverage": ("cov_det_union", "cov_det", "cov_ref_union", "cov_ref"),
    "one_to_one": ("det_height", "matched_det_height", "ref_height", "matched_ref_height"),
}


def bootstrap(rows, kind="one_to_one"):
    """Page bootstrap: 1,000 draws, seed 2107; 95% percentile intervals for P, R, F1."""
    rng = np.random.default_rng(SEED)
    n = len(rows)
    k = KEYS[kind]
    a = np.array([[r[k[0]], r[k[1]], r[k[2]], r[k[3]]] for r in rows], dtype=float)
    out = []
    for _ in range(DRAWS):
        s = a[rng.integers(0, n, n)].sum(axis=0)
        out.append(prf(s[1], s[0], s[3], s[2]))
    out = np.array(out)
    res = {}
    for i, key in enumerate(("precision", "recall", "f1")):
        lo, hi = np.percentile(out[:, i], [2.5, 97.5])
        res[key] = [round(float(lo), 4), round(float(hi), 4)]
    return res


def score(detections, refs):
    """detections: id -> {gutters, ...}; refs: id -> {gutters, excluded, width, height}."""
    rows, per_page = [], {}
    for pid in sorted(refs):
        ref = refs[pid]
        if pid not in detections:
            raise SystemExit(f"no detections for {pid}")
        d = detections[pid]
        s = match_page(d["gutters"], ref["gutters"], ref["width"], ref["excluded"])
        s["height_px"] = ref["height"]
        rows.append(s)
        per_page[pid] = {k: (round(v, 2) if isinstance(v, float) else v) for k, v in s.items()}
        p, r, f = prf(s["matched_det_height"], s["det_height"], s["matched_ref_height"], s["ref_height"])
        per_page[pid].update({"f1": round(f, 4), "precision": round(p, 4), "recall": round(r, 4)})
    p, r, f = pooled(rows)
    norm = [dict(r_, **{k: r_[k] / r_["height_px"] for k in
                        ("det_height", "matched_det_height", "ref_height", "matched_ref_height")}) for r_ in rows]
    pn, rn, fn = pooled(norm)
    return {
        "f1": f, "interval_95": bootstrap(rows) if rows else {}, "pages": len(rows),
        "per_page": per_page, "precision": p, "recall": r,
        "secondary_page_normalised": {"f1": fn, "precision": pn, "recall": rn},
        "totals": {k: sum(x[k] for x in rows) for k in
                   ("det_height", "det_ignored", "det_n", "matched_det_height", "matched_n",
                    "matched_ref_height", "ref_height", "ref_n")},
    }


# ---------------------------------------------------------------- several sets (second attempt)

def page_rows(detections, refs):
    """Per-page match sums for one set; detections: id -> {gutters}, refs: id -> reference page."""
    out = {}
    for pid in sorted(refs):
        if pid not in detections:
            raise SystemExit(f"no detections for {pid}")
        r = refs[pid]
        s = match_page(detections[pid]["gutters"], r["gutters"], r["width"], r["excluded"], r.get("zones"))
        s["height_px"] = r["height"]
        out[pid] = s
    return out


def _vec(rows, kind="primary"):
    k = KEYS[kind]
    return np.array([[r[k[0]], r[k[1]], r[k[2]], r[k[3]]] for r in rows], dtype=float).reshape(-1, 4)


def set_pr(v):
    """Precision and recall of one set from its pages' [det, matched det, ref, matched ref] heights."""
    s = v.sum(axis=0)
    return (s[1] / s[0] if s[0] > 0 else 0.0), (s[3] / s[2] if s[2] > 0 else 0.0)


def pool(vecs):
    """Equal-weight pool: precision and recall per set from that set's pooled heights, then the means
    over sets; F1 from the means. vecs: set name -> array of per-page rows."""
    prs = {k: set_pr(v) for k, v in sorted(vecs.items())}
    p = float(np.mean([x[0] for x in prs.values()]))
    r = float(np.mean([x[1] for x in prs.values()]))
    f = 2 * p * r / (p + r) if p + r > 0 else 0.0
    return p, r, f, prs


def pool_bootstrap(vecs):
    """Resample pages within each set (1,000 draws, seed 2107), recompute the pool."""
    rng = np.random.default_rng(SEED)
    names = sorted(vecs)
    res = []
    for _ in range(DRAWS):
        sub = {k: vecs[k][rng.integers(0, len(vecs[k]), len(vecs[k]))] for k in names if len(vecs[k])}
        p, r, f, _ = pool(sub)
        res.append((p, r, f))
    res = np.array(res)
    out = {}
    for i, k in enumerate(("precision", "recall", "f1")):
        lo, hi = np.percentile(res[:, i], [2.5, 97.5])
        out[k] = [round(float(lo), 4), round(float(hi), 4)]
    return out


def score_sets(sets, per_page=True):
    """sets: name -> {detections, refs, for_publication}.

    Primary figures (top level, and "precision"/"recall"/"f1" per set) are height coverage, the
    amended metric; "one_to_one" holds the first plan's matching as a secondary output.
    """
    rows = {name: page_rows(d["detections"], d["refs"]) for name, d in sorted(sets.items())}
    out = {}
    for kind in ("primary", "coverage", "one_to_one"):
        vecs = {n: _vec(list(r.values()), kind) for n, r in rows.items()}
        p, r, f, prs = pool(vecs)
        per_set = {}
        for name in sorted(sets):
            sp, sr = prs[name]
            per_set[name] = {"f1": 2 * sp * sr / (sp + sr) if sp + sr > 0 else 0.0,
                             "interval_95": bootstrap(list(rows[name].values()), kind),
                             "precision": sp, "recall": sr}
        out[kind] = {"f1": f, "interval_95": pool_bootstrap(vecs), "per_set": per_set,
                     "precision": p, "recall": r}
    res = dict(out["primary"])
    res["coverage"] = out["coverage"]
    res["one_to_one"] = out["one_to_one"]
    for name in sorted(sets):
        tot = {k: sum(x[k] for x in rows[name].values()) for k in
               ("a_det", "b_det", "ab_det", "cov_det", "cov_det_union", "cov_ref", "cov_ref_union", "det_height", "det_ignored", "det_n",
                "matched_det_height", "matched_n", "matched_ref_height", "ref_height", "ref_n")}
        e = res["per_set"][name]
        e.update({"for_publication": sets[name].get("for_publication", True), "pages": len(rows[name]),
                  "totals": tot})
        e["coverage"] = out["coverage"]["per_set"][name]
        e["one_to_one"] = out["one_to_one"]["per_set"][name]
        c_share = 1.0 - (tot["a_det"] + tot["b_det"]) / tot["cov_det_union"] if tot["cov_det_union"] > 0 else 0.0
        e["unscored_share_c"] = c_share
        if per_page and e["for_publication"]:
            e["per_page"] = {pid: {k: (round(v, 2) if isinstance(v, float) else v) for k, v in x.items()}
                             for pid, x in rows[name].items()}
    return res


def main_sets(a):
    spec = json.load(open(a.sets_json))
    sets = {}
    for name, d in sorted(spec.items()):
        ref = json.load(open(d["refs"]))
        det = {}
        for path in d["detections"]:
            x = json.load(open(path))
            x.pop("_meta", None)
            det.update(x)
        sets[name] = {"detections": det, "refs": ref["pages"], "for_publication": ref.get("for_publication", True)}
    res = score_sets(sets, per_page=not a.no_per_page)
    res["for_publication"] = all(v["for_publication"] for v in res["per_set"].values())
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True, indent=1)
    print(json.dumps({"f1": round(res["f1"], 4), "interval_95": res["interval_95"],
                      "precision": round(res["precision"], 4), "recall": round(res["recall"], 4),
                      "sets": {k: [round(v["precision"], 4), round(v["recall"], 4), round(v["f1"], 4)]
                               for k, v in res["per_set"].items()},
                      "one_to_one": [round(res["one_to_one"][k], 4) for k in ("precision", "recall", "f1")]},
                     sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--sets-json", help='several sets: {"NAME": {"refs": refs.json, "detections": [files]}}')
    ap.add_argument("--detections", nargs="+", help="shard files from columns.py (one set, first attempt)")
    ap.add_argument("--regions")
    ap.add_argument("--selection")
    ap.add_argument("--out", required=True)
    ap.add_argument("--only-ca-page", action="store_true")
    ap.add_argument("--no-per-page", action="store_true")
    a = ap.parse_args(argv)
    if a.sets_json:
        return main_sets(a)
    det, meta = {}, []
    for path in a.detections:
        with open(path) as f:
            d = json.load(f)
        meta.append(d.pop("_meta", {}))
        det.update(d)
    refs = refgutters.build(a.regions, a.selection, a.only_ca_page)
    res = score(det, refs)
    res["detection_meta"] = meta
    if a.no_per_page:
        res.pop("per_page")
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True, indent=1)
    print(json.dumps({k: (round(res[k], 4) if isinstance(res[k], float) else res[k])
                      for k in ("f1", "interval_95", "pages", "precision", "recall")}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
