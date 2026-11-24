#!/usr/bin/env python3
"""Banded v2 (#2107): the banded finder at 337fc0aa, then every gutter piece refitted for tilt and column rules.

Plan frozen before any run (issue #2107, comment 7122420362). The refit is the en2107-l2 refit
(enjudge-2107/step3/code/refit.py, sha256 f0d0b780...), ported unchanged; nothing is tuned here.

For each piece (one band core of a v1 gutter, [x0, x1, y0, y1] in the page's own pixels): pivot at the
piece's vertical centre (x_p = (x0 + x1) / 2, y_c = (y0 + y1) / 2); theta in [-2, +2] degrees, step 0.05;
offset at the pivot within +-max(1% of page width, half the gutter width), 1 px steps, each side stopping
halfway to any other v1 piece on the page that overlaps vertically. Line: x(y) = x_p + off + tan(theta)(y - y_c);
theta > 0 leans right going down. Dark = grayscale <= the page's Otsu threshold (full resolution, once per page).
  - rule fit: a run of 1-px sheared columns dark over >= 80% of the rows at some theta, no wider than
    max(6 px, 0.15% page width), lighter on both sides, inside the window; neighbouring columns at least 50%
    dark belong to the rule. The line is the rule's centre line; the rule's own dark pixels do not count.
  - white fit (no rule): the 3-px line with the fewest dark pixels; ties smallest |theta|, then smallest |offset|.
  - a piece with no rows keeps its v1 line ("fit": "none").

Scoring adapter (score.py scores vertical pieces): a refitted line is represented, in each scoring band, by
a piece of the v1 piece's width centred on the line's x at the band's vertical centre. The scoring band is the
piece's own y range (one band core of the finder), so a v1 line (theta 0, offset 0) maps to its own piece
exactly; check_adapter() verifies this on v1.

Method for the finder follows jscancella/NYTribuneOCRExperiments (findText_usingSums.py, GPL-3.0), reimplemented;
no code copied.
"""
import math

import numpy as np

THETAS = np.round(np.arange(-40, 41) * 0.05, 2)  # degrees
WIN_FRAC = 0.01
RULE_MIN_PX, RULE_FRAC, RULE_DARK, RULE_EDGE = 6, 0.0015, 0.8, 0.5
LINE_HALF = 1  # 3 px wide
DARK_TOUCH = 5  # descriptive: a line "touches print" with more than this many non-rule dark pixels
SETTINGS = {
    "theta_deg": [-2, 2, 0.05], "win_frac": WIN_FRAC, "rule_min_px": RULE_MIN_PX, "rule_frac": RULE_FRAC,
    "rule_dark": RULE_DARK, "rule_edge": RULE_EDGE, "line_half": LINE_HALF,
    "source": "en2107-l2 refit.py sha256 f0d0b78080b1e9774171381ef69aa192e63132999aa10886b4e12c2b23649593",
}


def otsu(gray):
    hist = np.bincount(gray.ravel(), minlength=256).astype(np.float64)
    p = hist / hist.sum()
    w0 = np.cumsum(p)
    mu = np.cumsum(p * np.arange(256))
    mt = mu[-1]
    with np.errstate(divide="ignore", invalid="ignore"):
        sb = (mt * w0 - mu) ** 2 / (w0 * (1 - w0))
    sb[~np.isfinite(sb)] = -1
    return int(np.argmax(sb))  # dark: gray <= t


def fit(ink, x, y0, y1, half_gutter, others):
    """The en2107-l2 fit, unchanged. others: x positions of other lines on the page overlapping this one vertically."""
    H, W = ink.shape
    r0, r1 = max(0, int(round(y0))), min(H, int(round(y1)))
    rows = np.arange(r0, r1)
    if rows.size == 0:
        return None
    yc = (y0 + y1) / 2.0
    R = int(math.ceil(max(WIN_FRAC * W, half_gutter or 0)))
    left, right = R, R
    for xo in others:
        d = xo - x
        if d >= 1:
            right = min(right, int(math.floor(d / 2)))
        elif d <= -1:
            left = min(left, int(math.floor(-d / 2)))
    offs = np.arange(-left, right + 1)
    k = np.arange(-left - LINE_HALF, right + LINE_HALF + 1)
    rule_w = max(RULE_MIN_PX, RULE_FRAC * W)
    best, vert, rules = None, None, []
    for th in THETAS:
        t = math.tan(math.radians(th))
        cx = x + t * (rows - yc)
        cols = np.rint(cx).astype(int)[:, None] + k[None, :]
        valid = (cols >= 0) & (cols < W)
        vals = np.where(valid, ink[rows[:, None], np.clip(cols, 0, W - 1)], False)
        colsum = vals.sum(axis=0).astype(np.int64)
        c = np.cumsum(np.concatenate([[0], colsum]))
        win = c[2 * LINE_HALF + 1:] - c[:-(2 * LINE_HALF + 1)]  # 3-wide sums, one per offset
        for oi, off in enumerate(offs):
            key = (int(win[oi]), abs(th), abs(int(off)))
            if best is None or key < best[0]:
                best = (key, float(th), int(off))
        if th == 0:
            vert = int(win[left])
        # Rules: runs of 1-px sheared columns dark over >= 80% of the rows, no wider than rule_w, inside the window.
        frac = colsum[LINE_HALF:LINE_HALF + len(offs)] / rows.size
        dark = frac >= RULE_DARK
        i = 0
        while i < len(dark):
            if dark[i]:
                j = i
                while j + 1 < len(dark) and dark[j + 1]:
                    j += 1
                if (j - i + 1) <= rule_w and i > 0 and j < len(dark) - 1:  # bounded by lighter columns on both sides
                    ea, eb = i, j  # the rule's soft edges (columns at least half dark) belong to the rule too
                    while ea - 1 >= 0 and frac[ea - 1] >= RULE_EDGE:
                        ea -= 1
                    while eb + 1 < len(frac) and frac[eb + 1] >= RULE_EDGE:
                        eb += 1
                    rules.append((float(frac[i:j + 1].mean()), float(th), (offs[i] + offs[j]) / 2.0, int(offs[ea]), int(offs[eb]), j - i + 1))
                i = j + 1
            else:
                i += 1

    def t_of(th):
        return math.tan(math.radians(th))
    if rules:
        rules.sort(key=lambda r: (-round(r[0], 3), abs(r[1]), abs(r[2])))
        mf, th, off, a, b, core = rules[0]
        t = t_of(th)
        # dark pixels of the 3-px line outside the rule's own columns
        cx = x + off + t * (rows - yc)
        lo, hi = x + a + t * (rows - yc), x + b + t * (rows - yc)
        cnt = 0
        for d in range(-LINE_HALF, LINE_HALF + 1):
            cc = np.rint(cx).astype(int) + d
            inside = (cc >= np.rint(lo)) & (cc <= np.rint(hi))
            ok = (cc >= 0) & (cc < W) & ~inside
            cnt += int(ink[rows[ok], cc[ok]].sum())
        return {"fit": "rule", "theta": th, "offset": off, "rule_width": core, "rule_width_with_edges": b - a + 1, "rule_dark_frac": round(mf, 3),
                "dark_best": cnt, "dark_vertical": vert, "rows": int(rows.size), "window": [-left, right],
                "x_top": x + off + t * (y0 - yc), "x_bottom": x + off + t * (y1 - yc)}
    (score, _, _), th, off = best
    t = t_of(th)
    return {"fit": "white", "theta": th, "offset": off, "dark_best": score, "dark_vertical": vert, "rows": int(rows.size),
            "window": [-left, right], "x_top": x + off + t * (y0 - yc), "x_bottom": x + off + t * (y1 - yc)}


def page_ink(full_gray):
    """Otsu threshold and dark mask of a full-resolution grayscale page."""
    thr = otsu(full_gray)
    return thr, full_gray <= thr


def refit_gutters(ink, gutters):
    """v1 gutters (lists of [x0, x1, y0, y1], page pixels; ink must be the page at the same pixels) -> v2 lines.

    Returns a list of gutters, each a list of line dicts: x0, x1, y0, y1 (the v1 piece), x_top, x_bottom (the
    fitted line's x at y0 and y1), half (half the v1 width) and the fit details.
    """
    pieces = [(gi, si, s) for gi, g in enumerate(gutters) for si, s in enumerate(g)]
    out = [[None] * len(g) for g in gutters]
    for gi, si, s in pieces:
        x0, x1, y0, y1 = (float(v) for v in s)
        x = (x0 + x1) / 2.0
        # guard: every other v1 piece on the page that overlaps this one vertically (en2107-l2's rule)
        others = sorted({round((o[0] + o[1]) / 2.0, 2) for gj, sj, o in pieces if (gj, sj) != (gi, si)
                         and min(o[3], y1) - max(o[2], y0) > 0 and abs((o[0] + o[1]) / 2.0 - x) >= 1})
        r = fit(ink, x, y0, y1, (x1 - x0) / 2.0, others)
        if r is None:
            r = {"fit": "none", "theta": 0.0, "offset": 0, "dark_best": 0, "dark_vertical": 0, "rows": 0,
                 "x_top": x, "x_bottom": x}
        out[gi][si] = dict(r, x0=x0, x1=x1, y0=y0, y1=y1, half=(x1 - x0) / 2.0)
    return out


def v1_lines(gutters):
    """v1 gutters as lines (theta 0, offset 0), for the adapter check and the false-gutter scorer."""
    return [[{"x0": float(s[0]), "x1": float(s[1]), "y0": float(s[2]), "y1": float(s[3]), "half": (s[1] - s[0]) / 2.0,
              "x_top": (s[0] + s[1]) / 2.0, "x_bottom": (s[0] + s[1]) / 2.0, "theta": 0.0, "offset": 0, "fit": "v1"}
             for s in g] for g in gutters]


def line_x(ln, y):
    """x of a fitted line at height y (linear between (x_top, y0) and (x_bottom, y1))."""
    if ln["y1"] == ln["y0"]:
        return (ln["x_top"] + ln["x_bottom"]) / 2.0
    return ln["x_top"] + (ln["x_bottom"] - ln["x_top"]) * (y - ln["y0"]) / (ln["y1"] - ln["y0"])


def scoring_gutters(lines):
    """Adapter: lines -> score.py pieces. One scoring band per piece (its own y range); the piece takes the v1
    width, centred on the line's x at the band's vertical centre."""
    out = []
    for g in lines:
        segs = []
        for ln in g:
            xc = line_x(ln, (ln["y0"] + ln["y1"]) / 2.0)
            segs.append([xc - ln["half"], xc + ln["half"], ln["y0"], ln["y1"]])
        out.append(segs)
    return out


def check_adapter(gutters):
    """True when v1 gutters pass through the adapter unchanged (every coordinate equal)."""
    back = scoring_gutters(v1_lines(gutters))
    return len(back) == len(gutters) and all(
        len(a) == len(b) and all([float(v) for v in sa] == sb for sa, sb in zip(a, b)) for a, b in zip(gutters, back))


def touch_counts(lines):
    """Descriptive: pieces whose v1 / v2 line touches more than DARK_TOUCH dark pixels of non-rule ink."""
    ps = [ln for g in lines for ln in g if ln["fit"] != "none"]
    return {"pieces": len(ps), "v1_gt5": sum(ln["dark_vertical"] > DARK_TOUCH for ln in ps),
            "v2_gt5": sum(ln["dark_best"] > DARK_TOUCH for ln in ps),
            "fit_rule": sum(ln["fit"] == "rule" for ln in ps), "fit_white": sum(ln["fit"] == "white" for ln in ps)}
