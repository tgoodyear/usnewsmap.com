#!/usr/bin/env python3
"""Line fit for gutter candidates of the combined segmenter (#2107): tilt search and column-rule snap.

The search and the rule test follow the settings frozen for set en2107-l2 and for banded v2 (#2107 comments
7108565573 and 7122420362): angle within +-2 degrees in 0.05 degree steps, pivot at the piece's vertical centre,
sideways shift within +-max(1% of page width, half the gutter width), each side stopping halfway to any other
line on the page that overlaps vertically. A rule is a run of 1-px sheared columns dark over >= 80% of the rows,
no wider than max(6 px, 0.15% of page width) at full resolution, bounded by lighter columns; columns at least
50% dark beside it belong to it. With a rule in range the line is the rule's centre line and the rule's own ink
does not count; otherwise the 3-px line with the fewest dark pixels (ties: smallest |theta|, then |offset|).
Dark = gray <= the page's Otsu threshold.

Here the fit runs on the page cache's work-resolution array (pagecache.py, 2000 px wide), so pixel sizes are
scaled by work width / original width. It describes candidates (features) and places output lines; it is a
stand-in until the banded v2 finder (branch banded-v2-2107) is pooled as a detector of its own.
"""
import math

import numpy as np

THETAS = np.round(np.arange(-40, 41) * 0.05, 2)  # degrees; positive leans right going down
WIN_FRAC = 0.01
RULE_MIN_PX, RULE_FRAC, RULE_DARK, RULE_EDGE = 6, 0.0015, 0.8, 0.5
LINE_HALF = 1  # 3-px line


def otsu(gray):
    """Otsu threshold of a uint8 array; dark pixels are gray <= the returned value."""
    hist = np.bincount(gray.ravel(), minlength=256).astype(np.float64)
    p = hist / max(1.0, hist.sum())
    w0 = np.cumsum(p)
    mu = np.cumsum(p * np.arange(256))
    with np.errstate(divide="ignore", invalid="ignore"):
        sb = (mu[-1] * w0 - mu) ** 2 / (w0 * (1 - w0))
    sb[~np.isfinite(sb)] = -1
    return int(np.argmax(sb))


def fit(ink, x, y0, y1, half_gutter=None, others=(), px_scale=1.0, thetas=THETAS):
    """Fit one line over rows [y0, y1) of the boolean ink array near column x (all in array pixels).

    others: x positions of other lines overlapping vertically (the guard). px_scale: array pixels per original
    pixel (for the 6 px rule minimum). Returns a dict with fit ("rule" or "white"), theta, offset, x_top,
    x_bottom, dark_best (dark pixels on the fitted 3-px line, rule ink excluded), dark_vertical (the unmoved
    vertical line), rows, and for a rule its width and darkness; None when the row range is empty.
    """
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
    rule_w = max(RULE_MIN_PX * px_scale, RULE_FRAC * W)
    best, vert, rules = None, None, []
    for th in thetas:
        t = math.tan(math.radians(th))
        cols = np.rint(x + t * (rows - yc)).astype(int)[:, None] + k[None, :]
        valid = (cols >= 0) & (cols < W)
        vals = np.where(valid, ink[rows[:, None], np.clip(cols, 0, W - 1)], False)
        colsum = vals.sum(axis=0).astype(np.int64)
        c = np.cumsum(np.concatenate([[0], colsum]))
        win = c[2 * LINE_HALF + 1:] - c[:-(2 * LINE_HALF + 1)]
        oi = int(np.lexsort((np.abs(offs), win))[0])
        key = (int(win[oi]), abs(float(th)), abs(int(offs[oi])))
        if best is None or key < best[0]:
            best = (key, float(th), int(offs[oi]))
        if th == 0:
            vert = int(win[left])
        frac = colsum[LINE_HALF:LINE_HALF + len(offs)] / rows.size
        dark = frac >= RULE_DARK
        i = 0
        while i < len(dark):
            if dark[i]:
                j = i
                while j + 1 < len(dark) and dark[j + 1]:
                    j += 1
                if (j - i + 1) <= rule_w and i > 0 and j < len(dark) - 1:
                    ea, eb = i, j
                    while ea - 1 >= 0 and frac[ea - 1] >= RULE_EDGE:
                        ea -= 1
                    while eb + 1 < len(frac) and frac[eb + 1] >= RULE_EDGE:
                        eb += 1
                    rules.append((float(frac[i:j + 1].mean()), float(th), (offs[i] + offs[j]) / 2.0,
                                  int(offs[ea]), int(offs[eb]), j - i + 1))
                i = j + 1
            else:
                i += 1
    if vert is None:
        vert = 0
    if rules:
        rules.sort(key=lambda r: (-round(r[0], 3), abs(r[1]), abs(r[2])))
        mf, th, off, a, b, core = rules[0]
        t = math.tan(math.radians(th))
        cx = x + off + t * (rows - yc)
        lo, hi = x + a + t * (rows - yc), x + b + t * (rows - yc)
        cnt = 0
        for d in range(-LINE_HALF, LINE_HALF + 1):
            cc = np.rint(cx).astype(int) + d
            inside = (cc >= np.rint(lo)) & (cc <= np.rint(hi))
            ok = (cc >= 0) & (cc < W) & ~inside
            cnt += int(ink[rows[ok], cc[ok]].sum())
        return {"fit": "rule", "theta": th, "offset": float(off), "rule_width": core, "rule_dark_frac": round(mf, 3),
                "dark_best": cnt, "dark_vertical": vert, "rows": int(rows.size),
                "x_top": x + off + t * (y0 - yc), "x_bottom": x + off + t * (y1 - yc)}
    (_, _, _), th, off = best
    t = math.tan(math.radians(th))
    return {"fit": "white", "theta": th, "offset": float(off), "rule_width": 0, "rule_dark_frac": 0.0,
            "dark_best": best[0][0], "dark_vertical": vert, "rows": int(rows.size),
            "x_top": x + off + t * (y0 - yc), "x_bottom": x + off + t * (y1 - yc)}


def x_at(line, y):
    """x of a fitted line (x_top at y0, x_bottom at y1) at height y."""
    y0, y1 = line["y0"], line["y1"]
    if y1 <= y0:
        return line["x_top"]
    return line["x_top"] + (line["x_bottom"] - line["x_top"]) * (y - y0) / (y1 - y0)
