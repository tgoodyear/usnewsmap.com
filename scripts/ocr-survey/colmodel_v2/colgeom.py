"""Column regions from text boxes, the clean-geometry filters, and gutters from regions (#2107, v2).

The second layout model's target is the column region: a rectangle holding one run of body text
that reads top to bottom in one column, bounded left and right by gutters (or the page edge) and
above and below by anything that spans columns (a headline across columns, a rule, the masthead).
No source has such labels, so they are derived here, with one rule for every source:

  columnize(boxes) stacks body-text boxes (ALTO TextLines, or the body-text zones of a reference set)
  into columns. Boxes are visited top to bottom. A box joins the open stack it overlaps most in x,
  provided that overlap is at least MIN_XOVERLAP of the narrower of the two, the vertical gap to the
  stack's bottom is at most `max_vgap` px, and the joined stack stays no wider than WIDE_FACTOR times
  the median box width. A box wider than WIDE_FACTOR times the median width is "spanning": it joins
  no stack and closes every open stack it overlaps in x. Stacks with fewer than `min_members`
  members or a height under MIN_HEIGHT of the page are dropped. Region = the stack's bounding box.
  Finally, regions overlapping by more than MERGE_IOU are merged.

Gutters from regions (the evaluation adapter): the scorer's own reference rule (refgutters.py,
pair_gutters + merge_gutters, copied from layout_columns) applied to the region boxes. So a predicted
column map is scored exactly as if its regions were a reference set's text zones.

Clean-page filters for the weak English labels (all must hold; each is recorded per page):
  lines >= MIN_LINES                    ALTO TextLines with a box
  columns >= MIN_COLUMNS                derived column regions
  gutters >= 1                          side-by-side regions exist
  cross_share <= MAX_CROSS              height share of ALTO lines that cross a derived gutter, the
                                        cross_share of #2107 step 2 (PR #2116, signals.box_crosses:
                                        the line reaches at least 1% of page width past the gutter
                                        centre on both sides over at least half its height), plus
                                        body-size lines that a gutter stops just above and resumes
                                        just below (merged across the gutter; see cross_share)
  coverage >= MIN_COVERAGE              share of line height inside some column region
  width_consistency >= MIN_WIDTH_CONS   share of column-region height whose width is within
                                        WIDTH_TOL of the height-weighted median region width
  block_align >= MIN_BLOCK_ALIGN        share of TextBlocks (by area) with at least 90% of their
                                        area inside one column region, among blocks that overlap any
  english >= MIN_ENGLISH                share of words that are one of the, of, and, to, a, in, that,
                                        is, for, was (a crude language check; LoC titles are mostly
                                        English but not all)
  aspect within 1%                      ALTO Page WIDTH/HEIGHT against the image's aspect

Sources of method: cross_share and the line-crossing test follow #2107 step 2 (layout_alto/signals.py,
PR #2116), reimplemented; the gutter rule is #2112's refgutters.py, copied.
"""
import statistics

import refgutters

MIN_XOVERLAP = 0.5
WIDE_FACTOR = 1.6
MIN_HEIGHT = 0.03   # of page height
MERGE_IOU = 0.3

MIN_LINES = 50
MIN_COLUMNS = 2
MAX_CROSS = 0.02
MIN_COVERAGE = 0.70
WIDTH_TOL = 0.25
MIN_WIDTH_CONS = 0.80
MIN_BLOCK_ALIGN = 0.85
MIN_ENGLISH = 0.10
ASPECT_TOL = 0.01

FILTERS = (
    ("lines", ">=", MIN_LINES), ("columns", ">=", MIN_COLUMNS), ("gutters", ">=", 1),
    ("cross_share", "<=", MAX_CROSS), ("coverage", ">=", MIN_COVERAGE),
    ("width_consistency", ">=", MIN_WIDTH_CONS), ("block_align", ">=", MIN_BLOCK_ALIGN),
    ("english", ">=", MIN_ENGLISH), ("aspect_err", "<=", ASPECT_TOL),
)
ENGLISH_WORDS = {"the", "of", "and", "to", "a", "in", "that", "is", "for", "was"}


def _w(b):
    return b[2] - b[0]


def _h(b):
    return b[3] - b[1]


def _area(b):
    return max(0.0, _w(b)) * max(0.0, _h(b))


def _inter(a, b):
    return max(0.0, min(a[2], b[2]) - max(a[0], b[0])) * max(0.0, min(a[3], b[3]) - max(a[1], b[1]))


def _xover(a, b):
    return max(0.0, min(a[2], b[2]) - max(a[0], b[0]))


def _union(a, b):
    return [min(a[0], b[0]), min(a[1], b[1]), max(a[2], b[2]), max(a[3], b[3])]


def columnize(boxes, page_w, page_h, max_vgap, min_members=1):
    """Column regions [x0, y0, x1, y1] from body-text boxes (see the module docstring)."""
    boxes = [list(map(float, b)) for b in boxes if b and _w(b) > 0 and _h(b) > 0]
    if not boxes:
        return []
    med = statistics.median(_w(b) for b in boxes)
    wide = WIDE_FACTOR * med
    stacks = []  # {"box", "n", "open"}
    for b in sorted(boxes, key=lambda b: (b[1], b[0])):
        if _w(b) > wide:
            for s in stacks:
                if s["open"] and _xover(s["box"], b) > 0:
                    s["open"] = False
            continue
        best, best_ov = None, 0.0
        for s in stacks:
            if not s["open"]:
                continue
            sb = s["box"]
            ov = _xover(sb, b)
            if ov < MIN_XOVERLAP * min(_w(sb), _w(b)):
                continue
            if b[1] - sb[3] > max_vgap:
                continue
            if _w(_union(sb, b)) > wide:
                continue
            if ov > best_ov:
                best, best_ov = s, ov
        if best is None:
            stacks.append({"box": list(b), "n": 1, "open": True})
        else:
            best["box"] = _union(best["box"], b)
            best["n"] += 1
    regs = [s["box"] for s in stacks if s["n"] >= min_members and _h(s["box"]) >= MIN_HEIGHT * page_h]
    return merge_overlapping(regs)


def merge_overlapping(regs):
    regs = [list(r) for r in regs]
    changed = True
    while changed:
        changed = False
        for i in range(len(regs)):
            for j in range(i + 1, len(regs)):
                a, b = regs[i], regs[j]
                inter = _inter(a, b)
                if inter <= 0:
                    continue
                iou = inter / (_area(a) + _area(b) - inter)
                if iou > MERGE_IOU or inter >= 0.8 * min(_area(a), _area(b)):
                    regs[i] = _union(a, b)
                    del regs[j]
                    changed = True
                    break
            if changed:
                break
    return sorted(([round(v, 2) for v in r] for r in regs), key=lambda r: (r[0], r[1]))


def gutters_from_regions(regions, page_w, page_h):
    """Gutters (lists of [x0, x1, y0, y1] segments) from column regions, by the scorer's reference rule."""
    boxes = [tuple(r[:4]) for r in regions if _w(r) > 0 and _h(r) > 0]
    if len(boxes) < 2:
        return []
    return refgutters.merge_gutters(refgutters.pair_gutters(boxes), page_w, page_h)


# ---------------------------------------------------------------- filters

def _segments(gutters):
    return [((s[0] + s[1]) / 2.0, float(s[2]), float(s[3])) for g in gutters for s in g]


def box_crosses(box, segs, page_w, margin=0.01, min_frac=0.5):
    """True if a line box reaches margin * page_w past a gutter centre on both sides over min_frac of its height."""
    x0, y0, x1, y1 = box
    h = y1 - y0
    if not h > 0:
        return False
    m = margin * page_w
    ivs = sorted((max(y0, a), min(y1, b)) for cx, a, b in segs
                 if cx - x0 >= m and x1 - cx >= m and min(y1, b) > max(y0, a))
    if not ivs:
        return False
    covered, c0, c1 = 0.0, ivs[0][0], ivs[0][1]
    for a, b in ivs[1:]:
        if a > c1:
            covered += c1 - c0
            c0, c1 = a, b
        else:
            c1 = max(c1, b)
    covered += c1 - c0
    return covered >= min_frac * h


def _bridged(box, segs, page_w, bridge, tol=0.01):
    """True if a gutter (pieces at one centre x, within tol of page width) stops just above the box
    and resumes just below it, within `bridge` px, at an x the box straddles by 1% of page width."""
    x0, y0, x1, y1 = box
    h = y1 - y0
    m, t = 0.01 * page_w, tol * page_w
    above = [cx for cx, a, b in segs if y0 - bridge <= b <= y0 + h / 2 and cx - x0 >= m and x1 - cx >= m]
    if not above:
        return False
    return any(abs(cx - ca) <= t for cx, a, b in segs if y1 - h / 2 <= a <= y1 + bridge for ca in above)


def cross_share(lines, gutters, page_w, bridge=0.0, body_h=None):
    """Height share of lines crossing a gutter.

    A line crosses when a gutter covers at least half its height past 1% of page width on both sides
    (#2107 step 2's rule). With bridge > 0, a body-size line (height at most 1.5 * body_h) also crosses
    when a gutter stops within `bridge` px above it and resumes within `bridge` px below it at the
    same x: a line the OCR merged across the gutter breaks the column stacks, so the gutter shows
    above and below the line but not through it. Larger type (headlines spanning columns) is not
    tested this way."""
    segs = _segments(gutters)
    tot = cross = 0.0
    for b in lines:
        h = _h(b)
        if h <= 0:
            continue
        tot += h
        if box_crosses(b, segs, page_w):
            cross += h
        elif bridge > 0 and body_h and h <= 1.5 * body_h and _bridged(b, segs, page_w, bridge):
            cross += h
    return cross / tot if tot > 0 else None


def coverage(lines, regions):
    tot = inside = 0.0
    for b in lines:
        h = _h(b)
        if h <= 0:
            continue
        tot += h
        a = _area(b)
        if a > 0 and any(_inter(b, r) >= 0.5 * a for r in regions):
            inside += h
    return inside / tot if tot > 0 else None


def width_consistency(regions):
    if not regions:
        return None
    pairs = sorted((_w(r), _h(r)) for r in regions)
    tot = sum(h for _, h in pairs)
    acc, med = 0.0, pairs[-1][0]
    for w, h in pairs:
        acc += h
        if acc >= tot / 2:
            med = w
            break
    ok = sum(h for w, h in pairs if abs(w - med) <= WIDTH_TOL * med)
    return ok / tot if tot > 0 else None


def block_align(blocks, regions):
    tot = ok = 0.0
    for b in blocks:
        a = _area(b)
        if a <= 0:
            continue
        inters = [_inter(b, r) for r in regions]
        if not inters or max(inters) <= 0:
            continue
        tot += a
        if max(inters) >= 0.9 * a:
            ok += a
    return ok / tot if tot > 0 else None


def english_share(words):
    words = [w.lower().strip(".,;:!?\"'()") for w in words]
    words = [w for w in words if w]
    if not words:
        return None
    return sum(1 for w in words if w in ENGLISH_WORDS) / len(words)


def judge(metrics):
    """(passed, [failed filter names]) for a page's metrics; a None metric fails its filter."""
    failed = []
    for name, op, lim in FILTERS:
        v = metrics.get(name)
        if v is None or (op == ">=" and v < lim) or (op == "<=" and v > lim):
            failed.append(name)
    return not failed, failed


def filter_spec():
    return [{"metric": n, "op": op, "limit": lim} for n, op, lim in FILTERS]
