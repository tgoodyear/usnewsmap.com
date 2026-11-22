#!/usr/bin/env python3
"""Reference column gutters from human region boxes (#2107, step 1).

No reference set has column labels, so a gutter is derived from the region boxes: two text regions
side by side whose vertical overlap is at least half the shorter one's height define a reference
gutter, the horizontal gap between them over their shared height.

Method for the finder this scores follows jscancella/NYTribuneOCRExperiments
(findText_usingSums.py, GPL-3.0), reimplemented; no code copied.

Datasets (the region boxes come from these):
  Reichsanzeiger-GT, CC BY 4.0, Mannheim University Library.
  American Stories gold, Apache-2.0 text, Dell Research Harvard, arXiv:2308.12477.
  Chronicling Germany, CC BY-NC 4.0, Schultze et al. 2025, University of Bonn.

Text regions (the only ones that define gutters): article, headline, paragraph, heading, caption,
image_caption. Excluded regions (ads, pictures, tables, mastheads, page headers): cartoon_or_advertisement,
advertisement, picture, image, figure, table, T<n>C<m> table cells, masthead, newspaper_header, header.
Every other type (page-number, footer, footnote, reference, author, other, inverted_text, none) is
neither: it defines no gutter and ignores no detection.

Reading choices where the plan is silent (see the PR):
  - Only adjacent pairs count: a pair is dropped if another text region touches the gap rectangle.
  - Gutters from different pairs in the same gap are merged into one: x ranges overlapping by at
    least half the narrower, y ranges overlapping or within MERGE_Y_TOL of the page height. The merged
    x range is the height-weighted mean of the parts, the y range their union.

    refgutters.py --regions regions.jsonl --selection selection.tsv --out refs.json [--only-ca-page]

Output is keyed JSON, keys sorted: id -> {excluded: [[x0, y0, x1, y1]], gutters: [[x0, x1, y0, y1]],
height, width}. Prints the per-set counts.
"""
import argparse
import csv
import json
import re
import sys
from collections import defaultdict

TEXT_TYPES = {"article", "headline", "paragraph", "heading", "caption", "image_caption"}
EXCLUDED_TYPES = {
    "cartoon_or_advertisement", "advertisement", "picture", "image", "figure", "table",
    "masthead", "newspaper_header", "header",
}
CELL = re.compile(r"T\d+C\d+$")
MIN_OVERLAP = 0.5  # of the shorter region's height
MERGE_X_OVERLAP = 0.5
MERGE_Y_TOL = 0.01  # of page height


def is_text(t):
    return t in TEXT_TYPES


def is_excluded(t):
    return t is not None and (t in EXCLUDED_TYPES or bool(CELL.match(t)))


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


def merge_gutters(gutters, page_height):
    """Merge gutters of the same gap into one (see module docstring)."""
    n = len(gutters)
    parent = list(range(n))

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    tol = MERGE_Y_TOL * page_height
    for i in range(n):
        for j in range(i + 1, n):
            a, b = gutters[i], gutters[j]
            xo = min(a[1], b[1]) - max(a[0], b[0])
            if xo <= 0 or xo < MERGE_X_OVERLAP * min(a[1] - a[0], b[1] - b[0]):
                continue
            if max(a[2], b[2]) - min(a[3], b[3]) > tol:
                continue
            parent[find(i)] = find(j)
    groups = defaultdict(list)
    for i in range(n):
        groups[find(i)].append(gutters[i])
    merged = []
    for g in groups.values():
        w = [max(1e-9, p[3] - p[2]) for p in g]
        tw = sum(w)
        x0 = sum(p[0] * wi for p, wi in zip(g, w)) / tw
        x1 = sum(p[1] * wi for p, wi in zip(g, w)) / tw
        merged.append([x0, x1, min(p[2] for p in g), max(p[3] for p in g)])
    merged.sort(key=lambda p: (p[0], p[2]))
    return merged


def page_reference(regions, page_height):
    tboxes = [tuple(r["box"]) for r in regions if is_text(r.get("type")) and r.get("box")]
    gut = merge_gutters(pair_gutters(tboxes), page_height)
    exc = [list(r["box"]) for r in regions if is_excluded(r.get("type")) and r.get("box")]
    return gut, exc


def read_selection(path, only_ca_page=False):
    sel = {}
    with open(path, newline="") as f:
        for row in csv.DictReader(f, delimiter="\t"):
            if only_ca_page and not row.get("ca_page"):
                continue
            sel[row["id"]] = row
    return sel


def build(regions_path, selection_path, only_ca_page=False):
    sel = read_selection(selection_path, only_ca_page)
    by_page = defaultdict(list)
    with open(regions_path) as f:
        for line in f:
            r = json.loads(line)
            if r["id"] in sel:
                by_page[r["id"]].append(r)
    refs = {}
    for pid, row in sorted(sel.items()):
        w, h = int(row["width"]), int(row["height"])
        gut, exc = page_reference(by_page.get(pid, []), h)
        refs[pid] = {"excluded": exc, "gutters": gut, "height": h, "width": w}
    return refs


def counts(refs):
    ng = sum(len(v["gutters"]) for v in refs.values())
    ht = sum(g[3] - g[2] for v in refs.values() for g in v["gutters"])
    return {"gutter_height_px": round(ht), "gutters": ng, "pages": len(refs),
            "pages_with_gutters": sum(1 for v in refs.values() if v["gutters"])}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--regions", required=True)
    ap.add_argument("--selection", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--only-ca-page", action="store_true",
                    help="keep only pages with a ca_page in selection.tsv (en-asgold: 7 pages)")
    a = ap.parse_args(argv)
    refs = build(a.regions, a.selection, a.only_ca_page)
    with open(a.out, "w") as f:
        json.dump(refs, f, sort_keys=True)
    print(json.dumps(counts(refs), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
