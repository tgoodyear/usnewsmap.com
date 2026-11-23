#!/usr/bin/env python3
"""ALTO-vs-columns layout signals and the planted line merge (#2107, step 2, frozen plan 2107-s2).

Method sources: step 1 of #2107 (banded column finder, #2112; method after jscancella/
NYTribuneOCRExperiments findText_usingSums.py, GPL-3.0, reimplemented); #2103 (render-vs-scan word
tables); Beyond Words (Library of Congress, CC0, biglam/loc_beyond_words@6c7f5fb3); American Stories
(Dell et al., arXiv:2308.12477).

A gutter is a list of segments [x0, x1, y0, y1] in page pixels (columns.py's format). A gutter's centre
at height y is the centre x of the segment that contains y.

Definitions, as frozen:
  crossing  A box crosses a gutter when the gutter's centre x lies inside the box and the box extends at
            least 1% of page width beyond the centre on both sides (equality counts), over at least half
            of the box's height (equality counts): the part of the box's y range, summed as a union, where
            some gutter segment has its centre that far inside the box. A box with no height never crosses.
  line_cross, word_cross  the test on a line's box and on a word's box. on_cross_line: the word's line crosses.
  cross_share  (page) sum of the heights of crossing lines over the sum of all line heights (lines with a box).
  missed_ink   (page) of the ink in the rows covered by at least one gutter (the detected columns), the share
            with no ALTO word box on it. Ink is columns.binarize with the frozen default Params. None when
            no gutter or no ink.
  block_split  (page) the TextBlocks whose centre lies in a detected column segment, divided by the number
            of column segments. A column segment is the run of rows with the same pair of neighbouring
            boundaries (a gutter, or the page edge) between two adjacent gutters (or an edge) where at least
            one gutter is active. None when no gutter.
  nearest gutter  of a line: among the gutters active at the line's vertical centre, the one whose centre x
            is closest to the line's right edge; ties go to the left (smaller x) gutter.
  planted merge  each line is merged with the line beside it across its nearest gutter into one box (union).
            Lines are visited by (vertical centre, x0); a line already merged is skipped. The partner is a
            not-yet-merged line whose centre lies on the other side of the gutter centre, whose vertical
            overlap with the line is at least half the shorter height, and whose near edge is closest to
            the gutter centre (ties: larger overlap, then lower index). No partner, no merge.
"""
import math


def segments(gutters):
    """Flat [(gutter id, centre x, y0, y1)] over all gutter segments."""
    out = []
    for gi, g in enumerate(gutters):
        for s in g:
            out.append((gi, (s[0] + s[1]) / 2.0, float(s[2]), float(s[3])))
    return out


def box_crosses(box, segs, page_w, margin=0.01, min_frac=0.5):
    """True if the box [x0, y0, x1, y1] crosses a gutter (see the module docstring)."""
    if box is None:
        return False
    x0, y0, x1, y1 = box
    h = y1 - y0
    if not h > 0:
        return False
    m = margin * page_w
    ivs = []
    for _, cx, sy0, sy1 in segs:
        if cx - x0 >= m and x1 - cx >= m:
            a, b = max(y0, sy0), min(y1, sy1)
            if b > a:
                ivs.append((a, b))
    if not ivs:
        return False
    ivs.sort()
    covered, cur0, cur1 = 0.0, ivs[0][0], ivs[0][1]
    for a, b in ivs[1:]:
        if a > cur1:
            covered += cur1 - cur0
            cur0, cur1 = a, b
        else:
            cur1 = max(cur1, b)
    covered += cur1 - cur0
    return covered >= min_frac * h


def line_flags(lines, segs, page_w):
    """{line idx: crosses} for the lines that have a box."""
    return {ln["idx"]: box_crosses(ln["box"], segs, page_w) for ln in lines if ln["box"] is not None}


def cross_share_of(boxes, segs, page_w):
    """Height-weighted share of line boxes that cross a gutter; None when there is no line height."""
    tot = cross = 0.0
    for b in boxes:
        if b is None or not b[3] > b[1]:
            continue
        h = b[3] - b[1]
        tot += h
        if box_crosses(b, segs, page_w):
            cross += h
    return cross / tot if tot > 0 else None


# ---------------------------------------------------------------- column segments

def column_segments(gutters, page_w, page_h):
    """Elementary column rectangles [(seg id, x0, x1, y0, y1)] over the rows where a gutter is active.

    Neighbouring elementary rectangles with the same (left gutter, right gutter) and touching rows share a
    segment id. Boundaries are gutter centres and the page edges."""
    segs = segments(gutters)
    if not segs:
        return [], 0
    ys = sorted({min(max(v, 0.0), float(page_h)) for s in segs for v in (s[2], s[3])})
    rects, open_ids, nseg = [], {}, 0
    for ya, yb in zip(ys, ys[1:]):
        if not yb > ya:
            continue
        mid = (ya + yb) / 2.0
        act = {}
        for gi, cx, sy0, sy1 in segs:
            if sy0 <= mid < sy1 and gi not in act:
                act[gi] = cx
        if not act:
            open_ids = {}
            continue
        order = sorted(act.items(), key=lambda kv: (kv[1], kv[0]))
        edges = [(None, 0.0)] + order + [(None, float(page_w))]
        new_open = {}
        for (lg, lx), (rg, rx) in zip(edges, edges[1:]):
            key = (lg, rg)
            sid = open_ids.get(key)
            if sid is None:
                sid = nseg
                nseg += 1
            new_open[key] = sid
            rects.append((sid, lx, rx, ya, yb))
        open_ids = new_open
    return rects, nseg


def block_split_of(blocks, rects, nseg):
    """TextBlocks whose centre is in a column segment, per column segment; None without segments."""
    if not nseg:
        return None
    n = 0
    for b in blocks:
        bx = b["box"]
        if bx is None:
            continue
        cx, cy = (bx[0] + bx[2]) / 2.0, (bx[1] + bx[3]) / 2.0
        if any(x0 <= cx < x1 and y0 <= cy < y1 for _, x0, x1, y0, y1 in rects):
            n += 1
    return n / nseg


def missed_ink_of(ink, word_boxes, rects, page_size):
    """Share of ink in gutter-covered rows with no word box on it. ink: bool array at work scale;
    word_boxes in page pixels; rects from column_segments; page_size (w, h) of the page image."""
    import numpy as np
    if not rects:
        return None
    hh, ww = ink.shape
    sx, sy = ww / float(page_size[0]), hh / float(page_size[1])
    rows = np.zeros(hh, dtype=bool)
    for _, _, _, ya, yb in rects:
        rows[int(math.floor(ya * sy)): int(math.ceil(yb * sy))] = True
    covered = np.zeros_like(ink)
    for b in word_boxes:
        x0, y0, x1, y1 = int(math.floor(b[0] * sx)), int(math.floor(b[1] * sy)), int(math.ceil(b[2] * sx)), int(math.ceil(b[3] * sy))
        covered[max(0, y0):max(0, y1), max(0, x0):max(0, x1)] = True
    live = ink & rows[:, None]
    tot = int(live.sum())
    if tot == 0:
        return None
    return float((live & ~covered).sum()) / tot


# ---------------------------------------------------------------- the page

def page_signals(parsed, gutters, page_size, ink=None):
    """cross_share, missed_ink, block_split of a page, plus the per-line flags."""
    segs = segments(gutters)
    w, h = page_size
    flags = line_flags(parsed["lines"], segs, w)
    rects, nseg = column_segments(gutters, w, h)
    boxes = [ln["box"] for ln in parsed["lines"]]
    wb = [wd["box"] for ln in parsed["lines"] for wd in ln["words"]]
    return {
        "cross_share": cross_share_of(boxes, segs, w),
        "block_split": block_split_of(parsed["blocks"], rects, nseg),
        "missed_ink": missed_ink_of(ink, wb, rects, page_size) if ink is not None else None,
        "line_cross": flags,
        "gutters": len(gutters),
        "lines": len(boxes),
    }


# ---------------------------------------------------------------- planted merge

def _active(gutters, y):
    out = []
    for gi, g in enumerate(gutters):
        for s in g:
            if s[2] <= y < s[3]:
                out.append((gi, (s[0] + s[1]) / 2.0))
                break
    return out


def plant_merge(boxes, gutters):
    """Merge lines across their nearest gutter. boxes: [x0, y0, x1, y1] or None. Returns (new boxes, merges)."""
    items = [(i, b) for i, b in enumerate(boxes) if b is not None and b[3] > b[1]]
    items.sort(key=lambda t: ((t[1][1] + t[1][3]) / 2.0, t[1][0], t[0]))
    used, out, merges = set(), [], 0
    for i, b in items:
        if i in used:
            continue
        act = _active(gutters, (b[1] + b[3]) / 2.0)
        if not act:
            continue
        gi, cx = min(act, key=lambda a: (abs(a[1] - b[2]), a[1]))
        left_side = (b[0] + b[2]) / 2.0 < cx
        best, best_key = None, None
        for j, c in items:
            if j == i or j in used:
                continue
            ccx = (c[0] + c[2]) / 2.0
            if (ccx > cx) != left_side:
                continue
            ov = min(b[3], c[3]) - max(b[1], c[1])
            if ov < 0.5 * min(b[3] - b[1], c[3] - c[1]):
                continue
            near = (c[0] - cx) if left_side else (cx - c[2])
            key = (abs(near), -ov, j)
            if best_key is None or key < best_key:
                best, best_key = j, key
        if best is None:
            continue
        c = boxes[best]
        out.append([min(b[0], c[0]), min(b[1], c[1]), max(b[2], c[2]), max(b[3], c[3])])
        used.update((i, best))
        merges += 1
    rest = [b for i, b in items if i not in used]
    return rest + out, merges


# ---------------------------------------------------------------- statistics

def auc(y, s):
    """P(a positive scores higher than a negative), ties half; None unless both classes are present."""
    import numpy as np
    from scipy.stats import rankdata
    y = np.asarray(y)
    s = np.asarray(s, dtype=float)
    pos = int(y.sum())
    neg = len(y) - pos
    if pos == 0 or neg == 0:
        return None
    ranks = rankdata(s)
    return float((ranks[y == 1].sum() - pos * (pos + 1) / 2) / (pos * neg))
