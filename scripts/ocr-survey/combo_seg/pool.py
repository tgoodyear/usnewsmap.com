#!/usr/bin/env python3
"""Candidate pooling for the combined column-gutter segmenter (#2107).

Every detector's gutters are cut into pieces; pieces from any detector that line up (x centres within 1% of
page width, the scorer's tolerance, and y ranges touching or within 1% of page height) are joined into one
candidate (union-find). A candidate is then cut into atoms: its height is split wherever the set of detectors
covering it changes, and atoms longer than MAX_ATOM of the page height are split evenly. The atom is the unit
that gets features, a label and a keep/drop decision, so a gutter can be kept down a column and dropped where
it crosses a headline or an advertisement.

Detector inputs are columns.py's gutter format (a gutter is a list of pieces [x0, x1, y0, y1]); a piece may
instead be a dict {"seg": [x0, x1, y0, y1], "x_top", "x_bottom", "fit", "theta", "conf"} (a fitted line, as the
banded v2 finder is to write them). Detector names, in the priority used to place an atom's starting x:
v2 (banded v2), v1 (banded v1, 337fc0aa), as (American Stories article gutters at 0.01), tr (the trained
YOLOv8m's article gutters at 0.25).
"""
import numpy as np

DETECTORS = ("v2", "v1", "as", "tr")
X_TOL = 0.01      # of page width: pieces closer than this in x line up (score.MARGIN)
Y_TOL = 0.01      # of page height: pieces closer than this in y line up (refgutters.MERGE_Y_TOL)
MAX_ATOM = 0.08   # of page height (the banded finder's band height)
MIN_ATOM = 0.004  # of page height: shorter atoms are dropped


def as_piece(p):
    """(x0, x1, y0, y1, extra dict) from a list piece or a dict piece."""
    if isinstance(p, dict):
        s = p["seg"]
        extra = {k: v for k, v in p.items() if k != "seg"}
    else:
        s, extra = p, {}
    return float(s[0]), float(s[1]), float(s[2]), float(s[3]), extra


def pieces(detections):
    """Flatten {detector: [gutter, ...]} into piece dicts with det, gid (gutter index), x0, x1, y0, y1, xc."""
    out = []
    for det in DETECTORS:
        for gi, g in enumerate(detections.get(det) or []):
            for p in g:
                x0, x1, y0, y1, extra = as_piece(p)
                if y1 <= y0:
                    continue
                xc = (x0 + x1) / 2.0
                if "x_top" in extra and "x_bottom" in extra:
                    xc = (extra["x_top"] + extra["x_bottom"]) / 2.0
                out.append(dict(extra, det=det, gid=gi, x0=x0, x1=x1, y0=y0, y1=y1, xc=xc))
    return out


def cluster(ps, width, height):
    """Union-find over pieces whose x centres are within X_TOL*W and whose y gap is at most Y_TOL*H."""
    n = len(ps)
    parent = list(range(n))

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    tx, ty = X_TOL * width, Y_TOL * height
    order = sorted(range(n), key=lambda i: ps[i]["xc"])
    for a_i, i in enumerate(order):
        for j in order[a_i + 1:]:
            if ps[j]["xc"] - ps[i]["xc"] > tx:
                break
            a, b = ps[i], ps[j]
            if max(a["y0"], b["y0"]) - min(a["y1"], b["y1"]) > ty:
                continue
            parent[find(i)] = find(j)
    # pieces of one detector gutter are not tied together by their gutter id: a gutter that drifts past the
    # tolerance splits into two candidates
    groups = {}
    for i in range(n):
        groups.setdefault(find(i), []).append(i)
    out = [sorted(g, key=lambda i: (ps[i]["y0"], ps[i]["xc"])) for g in groups.values()]
    out.sort(key=lambda g: (min(ps[i]["xc"] for i in g), min(ps[i]["y0"] for i in g)))
    return out


def atoms_of(ps, members, height, max_atom=MAX_ATOM, min_atom=MIN_ATOM):
    """Atoms (y0, y1, contributing piece indexes) of one candidate."""
    ys = sorted({ps[i]["y0"] for i in members} | {ps[i]["y1"] for i in members})
    spans = []
    for lo, hi in zip(ys[:-1], ys[1:]):
        cov = tuple(sorted(i for i in members if ps[i]["y0"] <= lo and ps[i]["y1"] >= hi))
        if not cov:
            continue
        dets = tuple(sorted({ps[i]["det"] for i in cov}))
        if spans and spans[-1][1] == lo and spans[-1][3] == dets:
            spans[-1] = (spans[-1][0], hi, tuple(sorted(set(spans[-1][2]) | set(cov))), dets)
        else:
            spans.append((lo, hi, cov, dets))
    out = []
    mx, mn = max_atom * height, min_atom * height
    for lo, hi, cov, _ in spans:
        n = max(1, int(np.ceil((hi - lo) / mx)))
        step = (hi - lo) / n
        for k in range(n):
            a, b = lo + k * step, lo + (k + 1) * step if k < n - 1 else hi
            if b - a < mn:
                continue
            c = tuple(i for i in cov if min(ps[i]["y1"], b) - max(ps[i]["y0"], a) > 0)
            out.append((a, b, c))
    return out


def start_x(ps, contrib, y0, y1):
    """Starting x and half width for an atom: the highest-priority contributing detector's piece."""
    rank = {d: k for k, d in enumerate(DETECTORS)}
    best = min(contrib, key=lambda i: (rank[ps[i]["det"]], ps[i]["xc"]))
    p = ps[best]
    ym = (y0 + y1) / 2.0
    if "x_top" in p and "x_bottom" in p and p["y1"] > p["y0"]:
        x = p["x_top"] + (p["x_bottom"] - p["x_top"]) * (ym - p["y0"]) / (p["y1"] - p["y0"])
    else:
        x = p["xc"]
    return x, (p["x1"] - p["x0"]) / 2.0, p["det"]


def pool_page(detections, width, height, max_atom=MAX_ATOM, min_atom=MIN_ATOM):
    """Candidates and atoms of one page.

    Returns (pieces, candidates): candidates is a list of {cid, members, atoms}, each atom a dict with
    y0, y1, contrib (piece indexes), dets (sorted detector names), x0_start, half_gutter, x_source.
    """
    ps = pieces(detections)
    cands = []
    for cid, members in enumerate(cluster(ps, width, height)):
        ats = []
        for y0, y1, contrib in atoms_of(ps, members, height, max_atom, min_atom):
            if not contrib:
                continue
            x, hg, src = start_x(ps, contrib, y0, y1)
            ats.append({"y0": y0, "y1": y1, "contrib": list(contrib), "dets": sorted({ps[i]["det"] for i in contrib}),
                        "x_start": x, "half_gutter": hg, "x_source": src})
        if ats:
            cands.append({"cid": cid, "members": members, "atoms": ats,
                          "dets": sorted({ps[i]["det"] for i in members}),
                          "y0": min(a["y0"] for a in ats), "y1": max(a["y1"] for a in ats)})
    return ps, cands
