#!/usr/bin/env python3
"""S2 of #2128: S0 (or any finder's output), with each box taller than 1.8 times the page's median line
height split at the minima of its row-ink profile.

For each page:
  - the median line height is the median height of the page's boxes (boxes under 8 px either way left
    out, as `chr_layout_qa.py` counts its merged-lines share);
  - a box taller than 1.8 times that is split into n = max(2, round(height / median)) pieces, if it is at
    least as wide as it is tall (a narrower box is a column rule or a page-edge smear, not merged lines:
    on the smoke pages such boxes, broken rules, were cut into 100 boxes each);
  - the row-ink profile is the number of ink pixels in each row of the box, on the same Sauvola-binarised
    scan S0 reads (`chr_layout_find.prepare`, window 51, k 0.3), smoothed with a moving mean
    max(3, round(median / 6)) rows wide;
  - a cut is a local minimum of the smoothed profile that is a real gap between lines: at most half the
    profile's median over the box, and at least half a median line height from the box's top and bottom
    and from the other cuts. The n - 1 lowest such minima are taken (lowest first); with fewer there are
    fewer cuts, and with none the box stays whole. (Without the depth test, a column rule or a page-edge
    smear 100 medians tall was cut into 100 boxes at the flat minima of its uniform ink.);
  - each piece keeps the box's left and right edges; its tid is the box's tid plus ".s<k>".

    s2_split.py --found found.jsonl --scans DIR --layout ghn|loc --out found-s2.jsonl [--jobs N]
"""
import argparse
import concurrent.futures as cf
import json
import os
import sys
import tempfile

import numpy as np

import common
import chr_layout_find as lf  # noqa: E402

FACTOR, DEPTH, WINDOW, K = 1.8, 0.5, 51, 0.3


def median_h(lines):
    hs = [l["box"][3] - l["box"][1] for l in lines if l["box"][2] - l["box"][0] >= 8 and l["box"][3] - l["box"][1] >= 8]
    return float(np.median(hs)) if hs else 0.0


def cuts_for(prof, n, med):
    """Up to n - 1 cut rows (indices into prof) at its lowest local minima, spaced half a median apart."""
    k = max(3, round(med / 6))
    s = np.convolve(prof, np.ones(k) / k, mode="same")
    h = len(s)
    margin = 0.5 * med
    deep = DEPTH * float(np.median(s))
    cand = [y for y in range(1, h - 1) if s[y] <= s[y - 1] and s[y] <= s[y + 1] and margin <= y <= h - margin
            and s[y] <= deep]
    cand.sort(key=lambda y: (s[y], y))
    picked = []
    for y in cand:
        if all(abs(y - p) >= margin for p in picked):
            picked.append(y)
            if len(picked) == n - 1:
                break
    return sorted(picked)


def split_page(lines, ink):
    """`ink`: bool array of the page, True on ink. Returns the new line list and how many boxes were split."""
    med = median_h(lines)
    out, n_split = [], 0
    for l in lines:
        x0, y0, x1, y1 = l["box"]
        h = y1 - y0
        if med <= 0 or h <= FACTOR * med or x1 - x0 < h:
            out.append(l)
            continue
        n = max(2, round(h / med))
        prof = ink[max(y0, 0):y1, max(x0, 0):x1].sum(axis=1).astype(float)
        cuts = cuts_for(prof, n, med)
        if not cuts:
            out.append(l)
            continue
        n_split += 1
        edges = [y0] + [y0 + c for c in cuts] + [y1]
        for k in range(len(edges) - 1):
            out.append({**l, "box": [x0, edges[k], x1, edges[k + 1]], "tid": f"{l.get('tid', '')}.s{k}"})
    return out, n_split


def do_page(args):
    rec, scan = args
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    with tempfile.TemporaryDirectory() as tmp:
        path = lf.prepare(scan, "sauvola", tmp, 1, WINDOW, K)
        ink = np.asarray(Image.open(path)) < 128
    lines, n_split = split_page(rec["lines"], ink)
    return {**rec, "lines": lines, "s2_split_boxes": n_split, "s2_median_h": median_h(rec["lines"])}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--found", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--layout", choices=["ghn", "loc"], required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=1)
    a = ap.parse_args()
    recs = common.read_jsonl(a.found)
    done = {json.loads(l)["page"] for l in open(a.out)} if os.path.exists(a.out) else set()
    todo = [(r, str(common.scan_path(a.scans, r["page"], a.layout))) for r in recs if r["page"] not in done]
    import multiprocessing as mp
    with open(a.out, "a") as out, cf.ProcessPoolExecutor(a.jobs, mp_context=mp.get_context("spawn"), max_tasks_per_child=1) as pool:
        for i, rec in enumerate(pool.map(do_page, todo), 1):
            out.write(json.dumps(rec, sort_keys=True) + "\n")
            out.flush()
            print(f"{i}/{len(todo)} {rec['page']} split {rec['s2_split_boxes']}", file=sys.stderr, flush=True)


if __name__ == "__main__":
    main()
