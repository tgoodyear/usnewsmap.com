#!/usr/bin/env python3
"""Beyond Words training pages for the combined segmenter (#2107): scans, negatives and ALTO, training only.

Only Beyond Words TRAIN rows are accepted (a validation row stops the script: those pages are the English test).

  --stage plan     For each page in --ids: its full-resolution Chronicling America scan, from Beyond Words'
                   source_url (bwfalse.scan_stem / iiif_url). A scan already in a --store-dir (bw-ca-scans on tank
                   first) or in --scan-dir is used as it is; the rest go in the fetch plan for fetch_scans.py.
  --stage finish   With the scans present: page cache (2,000 px, pagecache.to_gray), paths.json (page -> scan),
                   negatives.json (marked boxes scaled from Beyond Words' image to the scan, sx = W / width,
                   sy = H / height; bwfalse.marked_boxes' classes plus wide headlines), pages.json (ids kept) and
                   alto-map input. A page whose scan aspect differs from Beyond Words' image by more than 2% is left
                   out and counted (bwfalse's check), as is a page with no scan (fetch gave up).
Data: biglam/loc_beyond_words (CC0); Library of Congress, Chronicling America (scans).

    bw_train.py --stage plan   --rows BW_TRAIN.jsonl --ids train-subset-bw.txt --store-dir D [--store-dir D2]
                               --scan-dir D --out-dir D
    bw_train.py --stage finish --rows BW_TRAIN.jsonl --ids train-subset-bw.txt --store-dir D --scan-dir D
                               --out-dir D [--workers 2]
"""
import argparse
import collections
import json
import multiprocessing as mp
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
for d in (HERE, os.path.join(HERE, "..", "layout_columns"), os.path.join(HERE, "..", "layout_diag")):
    sys.path.insert(0, d)
import bwfalse  # noqa: E402
import pagecache  # noqa: E402

ASPECT_TOL = bwfalse.ASPECT_TOL


def rows_for(path, ids):
    want = set(ids)
    out = {}
    for r in bwfalse.read_rows(path):
        if str(r["page_id"]) in want:
            if r.get("split") != "train":
                raise SystemExit("not a Beyond Words training row: %s" % r["page_id"])
            out[str(r["page_id"])] = r
    missing = want - set(out)
    if missing:
        raise SystemExit("ids not in the rows: %s" % sorted(missing)[:5])
    return out


def source_url(r):
    m = bwfalse.lit(r["meta"])
    return m.get("source_url") or ""


def held(store_dirs, scan_dir):
    return bwfalse.have_scans([d for d in list(store_dirs or []) + [scan_dir] if d and os.path.isdir(d)], None)


def plan(a, rows, ids):
    have = held(a.store_dir, a.scan_dir)
    need, st = [], collections.Counter()
    for pid in ids:
        u = source_url(rows[pid])
        if "/data/" not in u:
            st["no source_url"] += 1
            continue
        stem = bwfalse.scan_stem(u)
        if stem in have:
            st["held"] += 1
            continue
        need.append({"page_id": pid, "stem": stem, "url": bwfalse.iiif_url(*bwfalse.scan_tail(u))})
    st["to_fetch"] = len(need)
    json.dump({"counts": dict(st), "fetch": need}, open(os.path.join(a.out_dir, "fetch-plan.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(dict(st), sort_keys=True))
    return 0


def _cache(job):
    pid, src, cache_dir = job
    out = os.path.join(cache_dir, "%s.2000.npz" % pid)
    if os.path.exists(out):
        z = np.load(out)
        return pid, [int(z["size"][0]), int(z["size"][1])]
    with open(src, "rb") as f:
        g, size = pagecache.to_gray(f.read(), 2000)
    pagecache.save(cache_dir, pid, 2000, g, size)
    return pid, [int(size[0]), int(size[1])]


def finish(a, rows, ids):
    have = held(a.store_dir, a.scan_dir)
    cache = os.path.join(a.out_dir, "cache")
    os.makedirs(cache, exist_ok=True)
    st = collections.Counter()
    jobs, src = [], {}
    for pid in ids:
        u = source_url(rows[pid])
        stem = bwfalse.scan_stem(u) if "/data/" in u else None
        if stem is None or stem not in have:
            st["left out: no scan"] += 1
            continue
        src[pid] = have[stem][0]
        jobs.append((pid, src[pid], cache))
    sizes = {}
    with mp.get_context("fork").Pool(a.workers) as pool:
        for n, (pid, size) in enumerate(pool.imap_unordered(_cache, jobs), 1):
            sizes[pid] = size
            if n % 50 == 0 or n == len(jobs):
                print("cache %d/%d" % (n, len(jobs)), flush=True)
    paths, negs, kept, left = {}, {}, [], []
    for pid in ids:
        if pid not in sizes:
            left.append({"page_id": pid, "why": "no scan"})
            continue
        r = rows[pid]
        W, H = sizes[pid]
        sx, sy = W / r["width"], H / r["height"]
        if abs(sx / sy - 1) > ASPECT_TOL:
            st["left out: aspect mismatch with the full scan"] += 1
            left.append({"page_id": pid, "why": "aspect"})
            continue
        boxes = []
        for k in ("nontext_regions", "text_regions"):
            for x in bwfalse.lit(r[k]):
                t = (x.get("type") or "").lower()
                x0, y0, x1, y1 = x["box"]
                if x1 <= x0 or y1 <= y0:
                    continue
                if t in bwfalse.NONTEXT or (t == "headline" and (x1 - x0) >= bwfalse.HEADLINE_MIN_W * r["width"]):
                    boxes.append([x0 * sx, y0 * sy, x1 * sx, y1 * sy, "headline" if t == "headline" else t])
        negs[pid] = boxes
        paths[pid] = src[pid]
        kept.append(pid)
    st["kept"] = len(kept)
    json.dump(paths, open(os.path.join(a.out_dir, "paths.json"), "w"), sort_keys=True)
    json.dump(negs, open(os.path.join(a.out_dir, "negatives.json"), "w"), sort_keys=True)
    json.dump({"pages": {p: {"width": sizes[p][0], "height": sizes[p][1]} for p in kept}},
              open(os.path.join(a.out_dir, "pages.json"), "w"), sort_keys=True)
    json.dump({p: {"source_url": source_url(rows[p]), "width": sizes[p][0], "height": sizes[p][1]} for p in kept},
              open(os.path.join(a.out_dir, "scan-sizes.json"), "w"), sort_keys=True)
    json.dump({"counts": dict(st), "left_out": left}, open(os.path.join(a.out_dir, "finish.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(dict(st), sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--stage", required=True, choices=("plan", "finish"))
    ap.add_argument("--rows", required=True)
    ap.add_argument("--ids", required=True)
    ap.add_argument("--store-dir", action="append", default=[])
    ap.add_argument("--scan-dir", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--workers", type=int, default=2)
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    ids = [ln.strip() for ln in open(a.ids) if ln.strip()]
    rows = rows_for(a.rows, ids)
    return plan(a, rows, ids) if a.stage == "plan" else finish(a, rows, ids)


if __name__ == "__main__":
    sys.exit(main())
