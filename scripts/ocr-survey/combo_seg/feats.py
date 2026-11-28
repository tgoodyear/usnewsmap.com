#!/usr/bin/env python3
"""Features and labels for the combined segmenter's atoms (#2107).

For each atom from pool.py the line is fitted (lines.py) and described by cheap features:
  - detectors: which drew it (det_*), how many, how many anywhere on its candidate, the candidate's length,
    the start piece's gap width, and for American Stories (as) and the trained YOLOv8m (tr) the confidence of
    the `article` boxes flanking the line on each side (the weaker side);
  - text: word and text-line boxes the fitted line cuts (Tesseract TSV, or LoC ALTO for Chronicling America
    pages), the share of the atom's height spent inside cut words, words close on each side;
  - ink: dark pixels on the fitted line and on the unmoved vertical line, per row; ink on each side;
  - fit: rule or white fit, |tilt|, |shift|, rule darkness;
  - layout: spacing to the nearest candidate on each side, its symmetry, and the spacing relative to the page
    median; atom height and position;
  - non-text: per class group, the highest confidence of a box of that group that the line crosses (as, tr).
Missing inputs are NaN (the GBDT handles them natively).

Labels (one per atom, from its output piece, the fitted line's x range over the atom's height):
  - reference pages: the piece's matched share is the share of its height lying inside some reference gap
    widened by 1% of page width (score.py's matching tolerance, applied to the piece as score.coverage_pieces
    does); its text share is the share inside a body-text zone shrunk by 1% (score.text_pieces). Positive if
    the matched share >= 0.5; negative if the text share >= 0.5; otherwise unlabelled (the scorer's unscored
    zone (c)). An atom more than half inside an excluded region is unlabelled (the scorer ignores it).
    Chronicling Germany references (cg_refs.py, the Europeana rule) mark only vertical separators as gutters, so
    its positives are ruled gutters only; un-ruled gaps there fall outside the zones and stay unlabelled.
  - negatives-only pages (Beyond Words): negative if at least half the line's height lies inside a marked
    non-text box (headlines only when at least 25% of the page width wide, as in the banded v2 plan's step 2);
    otherwise unlabelled.
  - volunteer lines (en-gutter): a settled yes or no line labels the atoms it lies on (vertical overlap at least
    half the atom, x within 1% of page width at the atom's centre).
"""
import math
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import lines  # noqa: E402
import pool  # noqa: E402
import score  # noqa: E402

GROUPS = {
    "article": {"article"},
    "headline": {"headline"},
    "ad": {"cartoon_or_advertisement"},
    "photo": {"photograph"},
    "mast": {"masthead", "newspaper_header"},
    "table": {"table"},
    "caption": {"image_caption"},
}
BOX_DETS = ("as", "tr")
FEATURES = (
    [f"det_{d}" for d in pool.DETECTORS]
    + ["n_dets", "cand_n_dets", "cand_len", "atom_len", "y_mid", "x_mid", "start_half_gutter", "x_source_rank"]
    + [f"{d}_flank_conf" for d in BOX_DETS]
    + ["words_cut", "words_cut_share", "lines_cut", "words_left", "words_right", "word_density"]
    + ["dark_fit_per_row", "dark_vert_per_row", "ink_left", "ink_right", "ink_side_min"]
    + ["fit_rule", "abs_theta", "abs_offset", "rule_dark_frac", "rule_width"]
    + ["gap_left", "gap_right", "gap_asym", "gap_rel_median"]
    + [f"{d}_cross_{g}" for d in BOX_DETS for g in GROUPS] + ["as_cross_headline_wide", "tr_cross_headline_wide"]
)
WORD_PAD = 0.0005    # of page width: a word is cut when the line is inside it by more than this
NEAR = 0.02          # of page width: the side windows for words and ink
INNER = 0.003        # of page width: the side windows start this far from the line
FLANK = 0.02         # of page width: article box edges this close to the line flank it
WIDE_HEADLINE = 0.25  # of page width (banded v2 plan, step 2)


def _y_overlap(a0, a1, b0, b1):
    return max(0.0, min(a1, b1) - max(a0, b0))


def _span_union(ivs):
    tot, cur0, cur1 = 0.0, None, None
    for a, b in sorted(ivs):
        if cur1 is None or a > cur1:
            if cur1 is not None:
                tot += cur1 - cur0
            cur0, cur1 = a, b
        else:
            cur1 = max(cur1, b)
    if cur1 is not None:
        tot += cur1 - cur0
    return tot


def line_x(xt, xb, y0, y1, y):
    return xt if y1 <= y0 else xt + (xb - xt) * (y - y0) / (y1 - y0)


def crossing_share(boxes, xt, xb, y0, y1):
    """Share of the atom's height [y0, y1] where some box (x0, y0, x1, y1, ...) spans the line."""
    h = y1 - y0
    if h <= 0 or not len(boxes):
        return 0.0
    ivs = []
    for b in boxes:
        a, c = max(y0, b[1]), min(y1, b[3])
        if c <= a:
            continue
        xm = line_x(xt, xb, y0, y1, (a + c) / 2.0)
        if b[0] < xm < b[2]:
            ivs.append((a, c))
    return _span_union(ivs) / h


def max_conf_crossing(boxes, xt, xb, y0, y1, min_share=0.25):
    """Highest confidence of a box that spans the line over at least min_share of the atom's height."""
    best = 0.0
    h = y1 - y0
    for b in boxes:
        a, c = max(y0, b[1]), min(y1, b[3])
        if c - a < min_share * h:
            continue
        xm = line_x(xt, xb, y0, y1, (a + c) / 2.0)
        if b[0] < xm < b[2]:
            best = max(best, float(b[4]))
    return best


def flank_conf(boxes, xt, xb, y0, y1, width):
    """min(left, right): the best `article` box confidence whose right (left) edge is near the line."""
    h = y1 - y0
    tol, inner = FLANK * width, INNER * width
    left = right = 0.0
    for b in boxes:
        if b[5] != "article":
            continue
        if _y_overlap(b[1], b[3], y0, y1) < 0.25 * h:
            continue
        xm = line_x(xt, xb, y0, y1, (max(y0, b[1]) + min(y1, b[3])) / 2.0)
        if xm - tol <= b[2] <= xm + inner:
            left = max(left, float(b[4]))
        if xm - inner <= b[0] <= xm + tol:
            right = max(right, float(b[4]))
    return min(left, right)


def word_features(words, line_boxes, xt, xb, y0, y1, width):
    """Cut words and lines, the cut share of the atom's height, words close on each side, word density."""
    nan = float("nan")
    out = dict(words_cut=nan, words_cut_share=nan, lines_cut=nan, words_left=nan, words_right=nan, word_density=nan)
    if words is None:
        return out
    h = y1 - y0
    pad, near = WORD_PAD * width, NEAR * width
    w = words
    if len(w):
        sel = (w[:, 3] > y0) & (w[:, 1] < y1)
        w = w[sel]
    cut, ivs, nl, nr = 0, [], 0, 0
    for b in w:
        ym = (max(y0, b[1]) + min(y1, b[3])) / 2.0
        xm = line_x(xt, xb, y0, y1, ym)
        if b[0] + pad < xm < b[2] - pad:
            cut += 1
            ivs.append((max(y0, b[1]), min(y1, b[3])))
        elif xm - near <= b[2] <= xm:
            nl += 1
        elif xm <= b[0] <= xm + near:
            nr += 1
    lc = 0
    for b in (line_boxes if line_boxes is not None else []):
        if b[3] <= y0 or b[1] >= y1:
            continue
        xm = line_x(xt, xb, y0, y1, (max(y0, b[1]) + min(y1, b[3])) / 2.0)
        if b[0] + pad < xm < b[2] - pad:
            lc += 1
    k = max(1e-9, h / 1000.0)
    return dict(words_cut=float(cut), words_cut_share=_span_union(ivs) / h if h > 0 else 0.0, lines_cut=float(lc),
                words_left=nl / k, words_right=nr / k, word_density=len(w) / k)


def ink_side(ink, x, r0, r1, a, b):
    """Ink share of the vertical strip [x + a, x + b) over rows [r0, r1) (array pixels)."""
    W = ink.shape[1]
    c0, c1 = int(max(0, math.floor(x + a))), int(min(W, math.ceil(x + b)))
    if c1 <= c0 or r1 <= r0:
        return float("nan")
    return float(ink[r0:r1, c0:c1].mean())


def label_ref(piece, ref):
    """(label, matched share, text share, excluded share) of one output piece against a reference page."""
    h = piece[3] - piece[2]
    if h <= 0:
        return -1, 0.0, 0.0, 0.0
    m = score.MARGIN * ref["width"]
    ex = score.excluded_fraction([piece], ref.get("excluded") or [])
    rsegs = [s for g in ref["gutters"] for s in g]
    a_p = score.coverage_pieces([piece], rsegs, m)
    a = score.height_union(a_p) / h
    b_p = score.text_pieces([piece], ref.get("zones") or [], m)
    b = (score.height_union(a_p + b_p) - score.height_union(a_p)) / h if b_p else 0.0
    if ex > score.IGNORE_FRAC:
        return -1, a, b, ex
    if a >= 0.5:
        return 1, a, b, ex
    if b >= 0.5:
        return 0, a, b, ex
    return -1, a, b, ex


def label_negatives(xt, xb, y0, y1, neg_boxes, width):
    """0 if at least half the line lies inside a marked non-text box, else -1. neg_boxes: [x0, y0, x1, y1, cls]."""
    keep = [b for b in neg_boxes if b[4] != "headline" or (b[2] - b[0]) >= WIDE_HEADLINE * width]
    s = crossing_share(keep, xt, xb, y0, y1)
    return (0 if s >= 0.5 else -1), s


def label_lines(xt, xb, y0, y1, labelled, width):
    """Label from the nearest volunteer line overlapping the atom; -1 when none lies on it."""
    ym = (y0 + y1) / 2.0
    xm = line_x(xt, xb, y0, y1, ym)
    best = None
    for L in labelled:
        if _y_overlap(L["y0"], L["y1"], y0, y1) < 0.5 * (y1 - y0):
            continue
        d = abs(line_x(L["x_top"], L["x_bottom"], L["y0"], L["y1"], ym) - xm)
        if d <= score.MARGIN * width and (best is None or d < best[0]):
            best = (d, L["label"])
    return -1 if best is None else int(best[1])


def page_rows(pid, detections, size, gray, boxes=None, words=None, ref=None, neg_boxes=None, line_labels=None,
              max_atom=pool.MAX_ATOM, min_atom=pool.MIN_ATOM, set_name=None):
    """Feature rows for every atom of one page.

    detections: {detector: gutters} in original pixels; size: (W, H) original; gray: the work-resolution array;
    boxes: {"as": [[x0, y0, x1, y1, conf, class]], "tr": [...]}; words: {"words": [[x0, y0, x1, y1, ...]],
    "lines": [...]} or None. Exactly the inputs present decide which labels are written.
    """
    W, H = size
    s = gray.shape[1] / float(W)
    sy = gray.shape[0] / float(H)
    thr = lines.otsu(gray)
    ink = gray <= thr
    ps, cands = pool.pool_page(detections, W, H, max_atom, min_atom)
    boxes = boxes or {}
    wa = np.asarray([b[:4] for b in (words or {}).get("words", [])], dtype=float).reshape(-1, 4) if words is not None else None
    la = np.asarray([b[:4] for b in (words or {}).get("lines", [])], dtype=float).reshape(-1, 4) if words is not None else None
    rank = {d: k for k, d in enumerate(pool.DETECTORS)}
    all_atoms = [(ci, ai, a) for ci, c in enumerate(cands) for ai, a in enumerate(c["atoms"])]
    rows = []
    for ci, ai, a in all_atoms:
        c = cands[ci]
        y0, y1 = a["y0"], a["y1"]
        others = [o["x_start"] * s for cj, aj, o in all_atoms if cj != ci and _y_overlap(o["y0"], o["y1"], y0, y1) > 0
                  and abs(o["x_start"] - a["x_start"]) * s >= 1]
        f = lines.fit(ink, a["x_start"] * s, y0 * sy, y1 * sy, a["half_gutter"] * s, others)
        if f is None:
            continue
        xt, xb = f["x_top"] / s, f["x_bottom"] / s
        piece = [min(xt, xb) - 0.5, max(xt, xb) + 0.5, y0, y1]
        xm = (xt + xb) / 2.0
        h = y1 - y0
        r0, r1 = int(round(y0 * sy)), int(round(y1 * sy))
        nrow = max(1, f["rows"])
        ft = {f"det_{d}": float(d in a["dets"]) for d in pool.DETECTORS}
        il = ink_side(ink, xm * s, r0, r1, -NEAR * W * s, -INNER * W * s)
        ir = ink_side(ink, xm * s, r0, r1, INNER * W * s, NEAR * W * s)
        ft.update(n_dets=float(len(a["dets"])), cand_n_dets=float(len(c["dets"])), cand_len=(c["y1"] - c["y0"]) / H,
                  atom_len=h / H, y_mid=(y0 + y1) / 2.0 / H, x_mid=xm / W, start_half_gutter=a["half_gutter"] / W,
                  x_source_rank=float(rank[a["x_source"]]),
                  dark_fit_per_row=f["dark_best"] / nrow, dark_vert_per_row=f["dark_vertical"] / nrow,
                  ink_left=il, ink_right=ir, ink_side_min=min(il, ir),
                  fit_rule=float(f["fit"] == "rule"), abs_theta=abs(f["theta"]), abs_offset=abs(f["offset"]) / s / W,
                  rule_dark_frac=float(f["rule_dark_frac"]), rule_width=float(f["rule_width"]) / s / W)
        for d in BOX_DETS:
            bx = boxes.get(d)
            ft[f"{d}_flank_conf"] = flank_conf(bx, xt, xb, y0, y1, W) if bx is not None else float("nan")
            for g, cls in GROUPS.items():
                ft[f"{d}_cross_{g}"] = max_conf_crossing([b for b in bx if b[5] in cls], xt, xb, y0, y1) if bx is not None else float("nan")
            ft[f"{d}_cross_headline_wide"] = (max_conf_crossing([b for b in bx if b[5] == "headline" and b[2] - b[0] >= WIDE_HEADLINE * W],
                                                                xt, xb, y0, y1) if bx is not None else float("nan"))
        ft.update(word_features(wa, la, xt, xb, y0, y1, W))
        row = {"page_id": pid, "width": W, "height": H, "cid": c["cid"], "atom": ai, "y0": round(y0, 2), "y1": round(y1, 2),
               "x_top": round(xt, 2), "x_bottom": round(xb, 2), "piece": [round(v, 2) for v in piece],
               "dets": a["dets"], "fit": f["fit"], "weight": h / H, "label": -1, "label_source": None, "f": ft}
        if set_name:
            row["set"] = set_name
        if ref is not None:
            lab, sa, sb, se = label_ref(piece, ref)
            row.update(label=lab, label_source="ref" if lab >= 0 else None, a_share=round(sa, 4), b_share=round(sb, 4),
                       ex_share=round(se, 4))
        if neg_boxes is not None and row["label"] < 0:
            lab, sn = label_negatives(xt, xb, y0, y1, neg_boxes, W)
            row.update(label=lab, label_source="negatives" if lab >= 0 else None, neg_share=round(sn, 4))
        if line_labels is not None and row["label"] < 0:
            lab = label_lines(xt, xb, y0, y1, line_labels, W)
            row.update(label=lab, label_source="volunteer" if lab >= 0 else None)
        rows.append(row)
    _spacing(rows, W)
    return rows


def _spacing(rows, W):
    """Gaps to the nearest other atom on each side (overlapping vertically by half the shorter), in page widths."""
    xs = [(r["x_top"] + r["x_bottom"]) / 2.0 for r in rows]
    gaps = []
    for i, r in enumerate(rows):
        gl = gr = float("nan")
        for j, o in enumerate(rows):
            if o["cid"] == r["cid"]:
                continue
            ov = _y_overlap(r["y0"], r["y1"], o["y0"], o["y1"])
            if ov < 0.5 * min(r["y1"] - r["y0"], o["y1"] - o["y0"]):
                continue
            d = (xs[j] - xs[i]) / W
            if d > 0 and not (gr <= d):
                gr = d
            if d < 0 and not (gl <= -d):
                gl = -d
        gaps.append((gl, gr))
    fin = [g for pair in gaps for g in pair if g == g]
    med = float(np.median(fin)) if fin else float("nan")
    for r, (gl, gr) in zip(rows, gaps):
        r["f"]["gap_left"], r["f"]["gap_right"] = gl, gr
        both = gl == gl and gr == gr
        r["f"]["gap_asym"] = abs(gl - gr) / max(gl, gr) if both else float("nan")
        m = min(gl, gr) if both else (gl if gl == gl else gr)
        r["f"]["gap_rel_median"] = m / med if (m == m and med == med and med > 0) else float("nan")


def matrix(rows):
    """(X float array in FEATURES order, y labels, sample weights) for labelled rows."""
    X = np.array([[r["f"].get(k, float("nan")) for k in FEATURES] for r in rows], dtype=float).reshape(-1, len(FEATURES))
    y = np.array([r["label"] for r in rows], dtype=int)
    w = np.array([r.get("weight", 1.0) for r in rows], dtype=float)
    return X, y, w
