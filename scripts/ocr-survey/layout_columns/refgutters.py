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
  - A gutter is a chain of segments [x0, x1, y0, y1], one per pair, so its x range can drift down a
    skewed page. Segments of the same gap are chained when their y ranges are within MERGE_Y_TOL of
    the page height and their x centres within MERGE_X_TOL of the page width. Gutter height is the sum
    of its segments' heights (holes between segments do not count).

    refgutters.py --regions regions.jsonl --selection selection.tsv --out refs.json [--only-ca-page]

Output is keyed JSON, keys sorted: id -> {excluded: [[x0, y0, x1, y1]], gutters: [[[x0, x1, y0, y1], ...]],
height, width} (a gutter is a list of segments). Prints the per-set counts.
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
MERGE_X_TOL = 0.01  # of page width
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


def page_reference(regions, page_width, page_height):
    tboxes = [tuple(r["box"]) for r in regions if is_text(r.get("type")) and r.get("box")]
    gut = merge_gutters(pair_gutters(tboxes), page_width, page_height)
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
        gut, exc = page_reference(by_page.get(pid, []), w, h)
        refs[pid] = {"excluded": exc, "gutters": gut, "height": h, "width": w}
    return refs


def counts(refs):
    ng = sum(len(v["gutters"]) for v in refs.values())
    ht = sum(s[3] - s[2] for v in refs.values() for g in v["gutters"] for s in g)
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
