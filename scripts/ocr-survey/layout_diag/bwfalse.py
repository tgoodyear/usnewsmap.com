#!/usr/bin/env python3
"""Banded v2 step 2 (#2107): English false gutters from Beyond Words (descriptive). Plan: comment 7122420362.

Pages: all 712 Beyond Words validation pages, each run on its full-resolution Chronicling America scan. Scans
we hold are used as they are; the rest are fetched once from tile.loc.gov IIIF (full/full), at most one
request a second, generic User-Agent, each with a sha256 sidecar, never re-fetched and never deleted.
Beyond Words' boxes are scaled from its image to the scan (sx = W / width, sy = H / height); a page whose
scan aspect differs from Beyond Words' image by more than 2% is left out and counted (bwneg.py's check).
Marked boxes: photograph, illustration, comics/cartoon, editorial cartoon, map, advertisement, and headline
boxes at least 25% of the page width wide.
Measure, per method: pooled over pages, the share of gutter height in lines that lie inside a marked box,
a line counting as inside when at least half of its length is inside the union of the marked boxes. A line
is one gutter piece: a v2 piece is its fitted (possibly tilted) line, any other method's piece is the
vertical line at its x centre; a piece's height is its own y extent. Page bootstrap, 1,000 draws, seed 2107;
paired differences between methods on the same draws. Rule 2 (with v2eval.py's rule 1): share(v2) minus
share(v1) is no more than 0.005. Lower bound on false gutters only (Beyond Words marks no body text).
Methods: v1, v2, AS@0.01, the trained YOLOv8m @0.25, the combination at 31b624560 (v2common.py).
Disclosed: Beyond Words training pages were in the trained model's training data.

    bwfalse.py map    --rows VAL.jsonl --index index-full.jsonl --confirmed confirmed-sha256.txt --out bw-val-map.json
    bwfalse.py plan   --rows VAL.jsonl --map bw-val-map.json [--store-dir D ...] [--store-listing F ...] --out fetch-plan.json
    bwfalse.py fetch  --plan fetch-plan.json --scan-dir D [--store-dir D ...] [--store-listing F ...] --max-fetch N
    bwfalse.py run    --rows ROWS.jsonl --out-dir D --as-model M --weights W
                      (--map bw-val-map.json --scan-dir D ... | --bw-frames --path-from A --path-to B --check-pages N)
                      [--measure-run] [--threads 4]
    bwfalse.py score  --rows ROWS.jsonl --out-dir D [--measure-run]

Validation rows are refused by `run` and `score` without --measure-run (phase 2 only). For checks, --bw-frames
runs Beyond Words TRAIN rows on Beyond Words' own stored image (scale 1), N pages in sha256("2107:v2check:"+id)
order, and fetches nothing.
Data: biglam/loc_beyond_words (CC0); Library of Congress, Chronicling America (scans).
"""
import argparse
import ast
import collections
import datetime
import hashlib
import json
import os
import re
import sys
import time
import urllib.request

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import v2common  # noqa: E402
import refit  # noqa: E402
import score  # noqa: E402

NONTEXT = {"photograph", "illustration", "comics/cartoon", "editorial cartoon", "map", "advertisement"}
HEADLINE_MIN_W = 0.25
INSIDE = 0.5
ASPECT_TOL = 0.02
RULE2_MAX = 0.005
UA = "research-client/1.0"
CHECK_PREFIX = "2107:v2check:"
DOC = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
_last = [0.0]


def lit(v):
    return ast.literal_eval(v) if isinstance(v, str) else v


def read_rows(path):
    with open(path) as f:
        return [json.loads(ln) for ln in f if ln.strip()]


# ------------------------------------------------------------------ page mapping (bwmap.py, as a module)

def iiif_tail(iiif_base):
    """(lccn, reel, folder, file) from a scan-index row's iiif_base (enjudge-2107/select_pages.py)."""
    parts = iiif_base.split("/image-services/iiif/")[1].split("/")[0].split(":")
    rest = parts[parts.index("data") + 1:]
    return (rest[0], rest[1] if len(rest) == 4 else "", rest[-2], rest[-1].split(".")[0])


def path_tail(url):
    """(lccn, reel, folder, file) from a batch path .../data/<lccn>[/<reel>]/<folder>/<file>.<ext>"""
    rest = url.split("/data/", 1)[1].split("/")
    return (rest[0], rest[1] if len(rest) == 4 else "", rest[-2], rest[-1].split(".")[0])


def scan_tail(url):
    """(batch dir, lccn, reel, folder, file) from a Beyond Words source_url (bwneg.py's tail)."""
    batchdir = url.split("/data/", 1)[0].rstrip("/").split("/")[-1]
    return (batchdir,) + path_tail(url)


def scan_stem(url):
    return "__".join(x for x in scan_tail(url) if x)


def iiif_url(batchdir, lccn, reel, folder, file):
    st = batchdir.split("_")[0]
    parts = ["service:ndnp:%s:batch_%s:data:%s" % (st, batchdir, lccn)] + ([reel] if reel else []) + [folder, file]
    return "https://tile.loc.gov/image-services/iiif/%s/full/full/0/default.jpg" % ":".join(parts)


def bw_map(a):
    conf = {ln.strip() for ln in open(a.confirmed) if ln.strip() and not ln.startswith("#")}
    by_tail = collections.defaultdict(list)
    with open(a.index) as f:
        for ln in f:
            r = json.loads(ln)
            if r.get("resolution", "full") != "full" or not DOC.match(r["doc_id"]):
                continue
            by_tail[iiif_tail(r["iiif_base"])].append(r)
    st, out = collections.Counter(), {}
    for r in read_rows(a.rows):
        m = lit(r["meta"])
        u = m.get("source_url") or ""
        if "/data/" not in u:
            st["no source_url"] += 1
            continue
        t = path_tail(u)
        hits = by_tail.get(t, [])
        docs = sorted({h["doc_id"] for h in hits})
        held = sorted({h["doc_id"] for h in hits if h["sha256"] in conf})
        st["in index" if docs else "not in index"] += 1
        st["held"] += bool(held)
        st["several docs"] += len(docs) > 1
        out[r["page_id"]] = {"tail": list(t), "source_url": u, "ca_identity": m.get("ca_identity"), "docs": docs, "held": held,
                             "index_rows": [{k: h[k] for k in ("doc_id", "loc_url", "sha256", "batch", "bytes")} for h in hits]}
    json.dump(out, open(a.out, "w"), indent=0, sort_keys=True)
    print(json.dumps(dict(st, pages=len(out)), sort_keys=True))
    return 0


# ------------------------------------------------------------------ local store and fetcher

def have_scans(store_dirs, listings):
    """stem -> (where, bytes) for every .jpg/.jp2 scan held: in a directory, or named in an `ls -la` listing."""
    have = {}
    for d in store_dirs or []:
        for fn in os.listdir(d):
            stem, ext = os.path.splitext(fn)
            if ext in (".jpg", ".jp2"):
                have.setdefault(stem, (os.path.join(d, fn), os.path.getsize(os.path.join(d, fn))))
    for lf in listings or []:
        for ln in open(lf):
            p = ln.split()
            if len(p) >= 9 and os.path.splitext(p[-1])[1] in (".jpg", ".jp2"):
                have.setdefault(os.path.splitext(p[-1])[0], ("listing:%s:%s" % (os.path.basename(lf), p[-1]), int(p[4])))
    return have


def plan(a):
    m = json.load(open(a.map))
    rows = read_rows(a.rows)
    have = have_scans(a.store_dir, a.store_listing)
    need, held_local, held_elsewhere = [], [], []
    for r in rows:
        e = m[r["page_id"]]
        stem = scan_stem(e["source_url"])
        if stem in have:
            held_local.append(r["page_id"])
        elif e["held"]:
            held_elsewhere.append(r["page_id"])  # in our corpus: copy, never fetch
        else:
            need.append({"page_id": r["page_id"], "stem": stem, "url": iiif_url(*scan_tail(e["source_url"]))})
    jpg = [b for s, (w, b) in have.items() if w.endswith(".jpg")]
    mean = float(np.mean(jpg)) if jpg else None
    res = {"pages": len(rows), "held_in_store": len(held_local), "held_in_corpus_not_copied": held_elsewhere,
           "to_fetch": len(need), "store_jpg_scans": len(jpg), "store_jpg_mean_bytes": mean,
           "estimated_fetch_bytes": round(mean * len(need)) if mean else None,
           "fetch": need}
    json.dump(res, open(a.out, "w"), indent=1, sort_keys=True)
    print(json.dumps({k: v for k, v in res.items() if k != "fetch"}, indent=1, sort_keys=True))
    return 0


def fetch_one(url, path):
    if os.path.exists(path) or os.path.exists(path + ".json"):
        raise SystemExit("refusing to re-fetch " + path)
    wait = 1.0 - (time.time() - _last[0])
    if wait > 0:
        time.sleep(wait)
    _last[0] = time.time()
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=120) as r:
        data = r.read()
    tmp = path + ".part"
    with open(tmp, "wb") as f:
        f.write(data)
    os.rename(tmp, path)
    side = {"url": url, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data),
            "fetched_at": datetime.datetime.now(datetime.timezone.utc).isoformat(), "user_agent": UA}
    with open(path + ".json", "w") as f:
        json.dump(side, f, sort_keys=True)
    return side


def fetch(a):
    """Fetch the planned scans not already held (checked again here), at most --max-fetch of them."""
    p = json.load(open(a.plan))
    os.makedirs(a.scan_dir, exist_ok=True)
    have = have_scans(list(a.store_dir or []) + [a.scan_dir], a.store_listing)
    todo = [e for e in p["fetch"] if e["stem"] not in have][: a.max_fetch]
    log_path = os.path.join(a.scan_dir, "fetch-log.jsonl")
    t0 = time.time()
    for n, e in enumerate(todo, 1):
        side = fetch_one(e["url"], os.path.join(a.scan_dir, e["stem"] + ".jpg"))
        with open(log_path, "a") as f:
            f.write(json.dumps(dict(side, page_id=e["page_id"]), sort_keys=True) + "\n")
        print("fetched %d/%d %.0fs" % (n, len(todo), time.time() - t0), flush=True)
    print("fetched %d; %d planned scans were already held" % (len(todo), len(p["fetch"]) - len([e for e in p["fetch"] if e["stem"] not in have])))
    return 0


# ------------------------------------------------------------------ marked boxes and the measure

def marked_boxes(row):
    """Beyond Words boxes counted as not-a-gutter, in Beyond Words' image pixels."""
    out = []
    for k in ("nontext_regions", "text_regions"):
        for x in lit(row[k]):
            t = (x.get("type") or "").lower()
            x0, y0, x1, y1 = x["box"]
            if x1 <= x0 or y1 <= y0:
                continue
            if t in NONTEXT or (t == "headline" and (x1 - x0) >= HEADLINE_MIN_W * row["width"]):
                out.append([x0, y0, x1, y1])
    return out


def inside_length(ln, boxes):
    """Height (y extent) of the line's part lying inside the union of boxes; the line is straight, so the
    share of its height is the share of its length."""
    y0, y1 = ln["y0"], ln["y1"]
    xa, xb = ln["x_top"], ln["x_bottom"]
    ivs = []
    for bx0, by0, bx1, by1 in boxes:
        lo, hi = max(y0, by0), min(y1, by1)
        if hi <= lo:
            continue
        if y1 > y0 and xb != xa:  # x(y) = xa + (xb - xa)(y - y0)/(y1 - y0); keep bx0 <= x(y) <= bx1
            ya = y0 + (bx0 - xa) * (y1 - y0) / (xb - xa)
            yb = y0 + (bx1 - xa) * (y1 - y0) / (xb - xa)
            lo, hi = max(lo, min(ya, yb)), min(hi, max(ya, yb))
        elif not (bx0 <= xa <= bx1):
            continue
        if hi > lo:
            ivs.append((lo, hi))
    tot, cur0, cur1 = 0.0, None, None
    for a, b in sorted(ivs):
        if cur1 is None or a > cur1:
            if cur1 is not None:
                tot += cur1 - cur0
            cur0, cur1 = a, b
        else:
            cur1 = max(cur1, b)
    if cur1 is not None:
        tot += cur1 - cur0
    return tot


def page_false(lines, boxes):
    """[false height, all height] for one page and method."""
    fh = th = 0.0
    for g in lines:
        for ln in g:
            h = ln["y1"] - ln["y0"]
            if h <= 0:
                continue
            th += h
            if inside_length(ln, boxes) >= INSIDE * h:
                fh += h
    return [fh, th]


def method_lines(page):
    out = {"v1": refit.v1_lines(page["v1"]), "v2": page["v2_lines"]}
    for k in ("as", "yolo", "combo"):
        out[k] = refit.v1_lines(page[k])
    return out


def share(v):
    s = v.sum(axis=0)
    return float(s[0] / s[1]) if s[1] > 0 else 0.0


def bootstrap(vecs):
    """Page bootstrap (same draws for every method): per-method share intervals and paired differences."""
    rng = np.random.default_rng(score.SEED)
    names = sorted(vecs)
    n = len(next(iter(vecs.values())))
    draws = {k: [] for k in names}
    for _ in range(score.DRAWS):
        i = rng.integers(0, n, n)
        for k in names:
            draws[k].append(share(vecs[k][i]))
    draws = {k: np.array(v) for k, v in draws.items()}

    def ci(x):
        lo, hi = np.percentile(x, [2.5, 97.5])
        return [float(lo), float(hi)]
    res = {"share": {k: ci(draws[k]) for k in names}, "paired_diff": {}}
    for i, a in enumerate(names):
        for b in names[i + 1:]:
            res["paired_diff"]["%s_minus_%s" % (b, a)] = ci(draws[b] - draws[a])
    return res


# ------------------------------------------------------------------ run and score

def is_val(rows):
    return any(r.get("split") == "val" for r in rows)


def pick_pages(a, rows):
    if a.bw_frames:
        if is_val(rows):
            raise SystemExit("--bw-frames is for train-row checks only")
        order = sorted(rows, key=lambda r: (hashlib.sha256((CHECK_PREFIX + str(r["page_id"])).encode()).hexdigest(), r["page_id"]))
        return order[: a.check_pages]
    return rows


def run(a):
    rows = read_rows(a.rows)
    if is_val(rows) and not a.measure_run:
        raise SystemExit("validation rows: phase 2 only (--measure-run)")
    pages = pick_pages(a, rows)
    os.makedirs(a.out_dir, exist_ok=True)
    m = json.load(open(a.map)) if a.map else {}
    have = have_scans(a.scan_dir, None) if a.scan_dir else {}
    asd = v2common.ASDetector(a.as_model, a.threads)
    yd = v2common.TrainedDetector(a.weights, a.threads)
    out, skipped, t0 = {}, collections.Counter(), time.time()
    for n, r in enumerate(pages, 1):
        pid = str(r["page_id"])
        if a.bw_frames:
            src = r["image_path"]
            if a.path_from and src.startswith(a.path_from):
                src = a.path_to + src[len(a.path_from):]
            where = "bw-frame"
        else:
            stem = scan_stem(m[r["page_id"]]["source_url"])
            if stem not in have:
                raise SystemExit("no scan for %s (%s): fetch first" % (pid, stem))
            src, where = have[stem][0], "scan"
        gray, size, full = v2common.load_page(v2common.read_bytes(src))
        sx, sy = size[0] / r["width"], size[1] / r["height"]
        rec = {"size": list(size), "scale": [sx, sy], "source": where, "image": os.path.basename(src)}
        if abs(sx / sy - 1) > ASPECT_TOL:
            skipped["aspect mismatch with the full scan"] += 1
            out[pid] = dict(rec, skipped="aspect")
            continue
        v1 = v2common.run_v1(gray, size)
        thr, lines = v2common.run_v2(v1, full)
        asb = asd.boxes(gray, size)
        yb = yd.boxes(gray, size)
        asg = v2common.detector_gutters(asb, size, v2common.AS_CONF)
        rec.update({"v1": v1, "v2_lines": lines, "otsu": thr, "as": asg,
                    "yolo": v2common.detector_gutters(yb, size, v2common.YOLO_CONF),
                    "combo": v2common.combo_gutters(v1, asg, size), "as_boxes": asb, "yolo_boxes": yb})
        out[pid] = rec
        print("pages %d/%d %.0fs" % (n, len(pages), time.time() - t0), flush=True)
    meta = {"pages": len(pages), "skipped": dict(skipped), "refit": refit.SETTINGS, "as_sha256": v2common.AS_SHA,
            "trained_sha256": v2common.TRAINED_SHA, "as_conf": v2common.AS_CONF, "yolo_conf": v2common.YOLO_CONF,
            "validation": is_val(rows), "wall_seconds": round(time.time() - t0, 1)}
    json.dump({"_meta": meta, "pages": out}, open(os.path.join(a.out_dir, "bw-methods.json"), "w"), sort_keys=True)
    return 0


def score_run(a):
    rows = {str(r["page_id"]): r for r in read_rows(a.rows)}
    val = is_val(list(rows.values()))
    if val and not a.measure_run:
        raise SystemExit("validation rows: phase 2 only (--measure-run)")
    out_path = os.path.join(a.out_dir, "bw-results.json")
    if val:
        assert not os.path.exists(out_path), "already scored"
    saved = json.load(open(os.path.join(a.out_dir, "bw-methods.json")))["pages"]
    ids = sorted(p for p in saved if "skipped" not in saved[p])
    per = {k: [] for k in v2common.METHODS}
    nbox = 0
    for pid in ids:
        r, pg = rows[pid], saved[pid]
        sx, sy = pg["scale"]
        boxes = [[b[0] * sx, b[1] * sy, b[2] * sx, b[3] * sy] for b in marked_boxes(r)]
        nbox += len(boxes)
        for k, lines in method_lines(pg).items():
            per[k].append(page_false(lines, boxes))
    vecs = {k: np.array(v, dtype=float).reshape(-1, 2) for k, v in per.items()}
    res = {"pages": len(ids), "pages_skipped": len(saved) - len(ids), "marked_boxes": nbox, "validation": val,
           "methods": {k: {"share": share(v), "false_height": float(v[:, 0].sum()), "gutter_height": float(v[:, 1].sum())}
                       for k, v in vecs.items()}}
    res["bootstrap"] = bootstrap(vecs) if ids else {}
    d = res["methods"]["v2"]["share"] - res["methods"]["v1"]["share"]
    res["rule_2"] = {"v2_minus_v1": d, "max": RULE2_MAX, "holds": bool(d <= RULE2_MAX)}
    with open(out_path, "x" if val else "w") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    print(json.dumps(res, indent=1, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("map")
    s.add_argument("--rows", required=True)
    s.add_argument("--index", required=True)
    s.add_argument("--confirmed", required=True)
    s.add_argument("--out", required=True)
    p = sp.add_parser("plan")
    p.add_argument("--rows", required=True)
    p.add_argument("--map", required=True)
    p.add_argument("--store-dir", action="append")
    p.add_argument("--store-listing", action="append")
    p.add_argument("--out", required=True)
    f = sp.add_parser("fetch")
    f.add_argument("--plan", required=True)
    f.add_argument("--scan-dir", required=True)
    f.add_argument("--store-dir", action="append")
    f.add_argument("--store-listing", action="append")
    f.add_argument("--max-fetch", type=int, required=True)
    r = sp.add_parser("run")
    r.add_argument("--rows", required=True)
    r.add_argument("--out-dir", required=True)
    r.add_argument("--as-model", required=True)
    r.add_argument("--weights", required=True)
    r.add_argument("--map")
    r.add_argument("--scan-dir", action="append")
    r.add_argument("--bw-frames", action="store_true")
    r.add_argument("--path-from")
    r.add_argument("--path-to")
    r.add_argument("--check-pages", type=int, default=3)
    r.add_argument("--threads", type=int, default=4)
    r.add_argument("--measure-run", action="store_true")
    c = sp.add_parser("score")
    c.add_argument("--rows", required=True)
    c.add_argument("--out-dir", required=True)
    c.add_argument("--measure-run", action="store_true")
    a = ap.parse_args(argv)
    return {"map": bw_map, "plan": plan, "fetch": fetch, "run": run, "score": score_run}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
