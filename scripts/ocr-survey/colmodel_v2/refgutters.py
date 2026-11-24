"""Reference gutters from side-by-side boxes: pair_gutters and merge_gutters (#2107).

pair_gutters and merge_gutters COPIED unchanged from scripts/ocr-survey/layout_columns/refgutters.py at 7a303f45 (branch
layout-diag-2107, not merged to main). The scorer (layout_columns/score.py) builds its reference
gutters with these two functions, so the evaluation adapter turns predicted column regions into
gutters with the very same rule. Keep the two copies identical.
"""
from collections import defaultdict

MIN_OVERLAP = 0.5  # of the shorter region's height
MERGE_X_TOL = 0.01  # of page width
MERGE_Y_TOL = 0.01  # of page height


def pair_gutters(boxes):
    """Gutters [x0, x1, y0, y1] from adjacent side-by-side boxes (x0, y0, x1, y1)."""
    out = []
    n = len(boxes)
    for i in range(n):
        for j in range(n):
            a, b = boxes[i], boxes[j]
            if i == j or not a[2] < b[0]:
                continue
            y0, y1 = max(a[1], b[1]), min(a[3], b[3])
            short = min(a[3] - a[1], b[3] - b[1])
            if short <= 0 or y1 - y0 < MIN_OVERLAP * short:
                continue
            gx0, gx1 = a[2], b[0]
            blocked = False
            for k in range(n):
                if k == i or k == j:
                    continue
                c = boxes[k]
                if c[0] < gx1 and c[2] > gx0 and c[1] < y1 and c[3] > y0:
                    blocked = True
                    break
            if not blocked:
                out.append([gx0, gx1, y0, y1])
    return out


def merge_gutters(segments, page_width, page_height):
    """Chain the per-pair segments of one gap into gutters.

    A gutter is a list of segments [x0, x1, y0, y1] sorted by y with no y overlap; its x range may
    drift down the page (a skewed scan). Two segments are chained when their y ranges are within
    MERGE_Y_TOL of the page height and their x centres within MERGE_X_TOL of the page width.
    """
    n = len(segments)
    parent = list(range(n))

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    ty, tx = MERGE_Y_TOL * page_height, MERGE_X_TOL * page_width
    for i in range(n):
        for j in range(i + 1, n):
            a, b = segments[i], segments[j]
            if max(a[2], b[2]) - min(a[3], b[3]) > ty:
                continue
            if abs((a[0] + a[1]) / 2 - (b[0] + b[1]) / 2) > tx:
                continue
            parent[find(i)] = find(j)
    groups = defaultdict(list)
    for i in range(n):
        groups[find(i)].append(segments[i])
    out = []
    for g in groups.values():
        g.sort(key=lambda s: (s[2], s[3]))
        segs = []
        for x0, x1, y0, y1 in g:
            if segs and y0 < segs[-1][3]:
                if y1 <= segs[-1][3]:
                    continue
                y0 = segs[-1][3]
            segs.append([x0, x1, y0, y1])
        out.append(segs)
    out.sort(key=lambda g: (g[0][2], g[0][0]))
    return out
