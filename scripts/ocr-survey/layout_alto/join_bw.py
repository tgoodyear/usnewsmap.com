#!/usr/bin/env python3
"""Join Beyond Words pages to LoC ALTO pages in the local `locbulk` copy (#2107 step 2, test B).

Method sources: step 1 of #2107 (#2112) and its reference rows (#2113); #2103; Beyond Words (Library of
Congress, CC0, biglam/loc_beyond_words@6c7f5fb3, Lee et al., arXiv:2005.01583); American Stories
(Dell et al., arXiv:2308.12477).

    join_bw.py --refs-dir DIR --locbulk DIR --out-dir DIR [--workers 6] [--limit-batches N]

The plan's join: a Beyond Words page joins when exactly one ALTO page of the same LCCN, date and edition has an
aspect ratio (ALTO Page WIDTH / HEIGHT) within 1% of the Beyond Words image's (|r_alto / r_image - 1| <= 0.01).
Statuses: joined | ambiguous (two or more within 1%) | no_aspect_match | issue_not_in_tar | batch_not_in_locbulk.

Reading rule: each page's Chronicling America identity (`meta.ca_identity`) names its batch, and the issue
is looked up inside that batch's `<batch>.tar.bz2` only; a locbulk-wide search would mean reading all 2.3 TB.
Members are `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<k>/ocr.xml`. The tars are read as a stream and never
modified; an issue's members must be contiguous (checked, the run fails otherwise).

Writes OUT/join.json (page id -> status and counts, keys sorted), OUT/alto/<page id>.xml for joined pages,
and OUT/cache/<page id>.2000.npz (pagecache.py's format, from the page image in images.zip) for them.
Progress is printed as `batches i/N`.
"""
import argparse
import hashlib
import io
import json
import multiprocessing as mp
import os
import sys
import tarfile
import time
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import altolayout  # noqa: E402

TOL = 0.01


def sha_key(page_id):
    return hashlib.sha256(("2107:s2:" + page_id).encode()).hexdigest()


def read_refs(refs_dir):
    rows = []
    for split in ("train", "val"):
        with open(os.path.join(refs_dir, f"biglam__loc_beyond_words__{split}.jsonl")) as f:
            for ln in f:
                r = json.loads(ln)
                ci = r["meta"]["ca_identity"]
                rows.append({"page_id": r["page_id"], "split": r["split"], "batch": ci["batch"], "lccn": ci["lccn"],
                             "date": ci["date"], "edition": int(ci["edition"]), "width": r["width"], "height": r["height"],
                             "image_path": r["image_path"]})
    return rows


def decide(img_w, img_h, group):
    """group: {seq: (alto_w, alto_h)} of one issue. Returns (status, chosen seq or None, n within tolerance)."""
    if not group:
        return "issue_not_in_tar", None, 0
    # |r_alto / r_image - 1| <= 1%, as a cross-multiplication so the 1% edge is exact: |w*ih - iw*h| * 100 <= iw*h
    near = sorted(s for s, (w, h) in group.items() if h > 0 and abs(w * img_h - img_w * h) * 100 <= img_w * h)
    if len(near) == 1:
        return "joined", near[0], 1
    return ("ambiguous" if near else "no_aspect_match"), None, len(near)


def parse_member(name):
    p = name.split("/")
    if len(p) != 7 or p[6] != "ocr.xml" or not p[4].startswith("ed-") or not p[5].startswith("seq-"):
        return None
    try:
        return (p[0], f"{p[1]}-{p[2]}-{p[3]}", int(p[4][3:])), p[5]
    except ValueError:
        return None


def scan_batch(args):
    """One tar: returns (batch, {page id: record}, [(page id, alto bytes)])."""
    batch, tar_path, pages, out_alto = args
    wanted = {}
    for r in pages:
        wanted.setdefault((r["lccn"], r["date"], r["edition"]), []).append(r)
    res, finished, cur, group = {}, set(), None, {}

    def finish(key):
        for r in wanted.get(key, []):
            dims = {s: (d[0], d[1]) for s, d in group.items()}
            status, seq, n = decide(r["width"], r["height"], dims)
            res[r["page_id"]] = {"status": status, "batch": batch, "lccn": r["lccn"], "date": r["date"],
                                 "edition": r["edition"], "issue_pages": len(dims), "within_1pct": n,
                                 "seq": seq, "split": r["split"]}
            if status == "joined":
                with open(os.path.join(out_alto, r["page_id"] + ".xml"), "wb") as f:
                    f.write(group[seq][2])

    with tarfile.open(tar_path, "r|bz2") as tf:
        for m in tf:
            pm = parse_member(m.name)
            if pm is None:
                continue
            key, seq = pm
            if key != cur:
                if cur is not None:
                    finish(cur)
                    finished.add(cur)
                    group = {}
                if key in finished:
                    raise RuntimeError(f"{batch}: issue {key} is not contiguous in the tar")
                cur = key
            if key in wanted:
                data = tf.extractfile(m).read()
                d = altolayout.page_dims(data)
                if d:
                    group[seq] = (d[0], d[1], data)
        if cur is not None:
            finish(cur)
            finished.add(cur)
    for key, rs in wanted.items():
        for r in rs:
            if r["page_id"] not in res:
                res[r["page_id"]] = {"status": "issue_not_in_tar", "batch": batch, "lccn": r["lccn"], "date": r["date"],
                                     "edition": r["edition"], "issue_pages": 0, "within_1pct": 0, "seq": None,
                                     "split": r["split"]}
    return batch, res


def cache_pages(rows_by_id, joined, out_cache, work_width=2000):
    import pagecache
    zips = {}
    for pid in joined:
        path, member = rows_by_id[pid]["image_path"].split("!", 1)
        if path not in zips:
            zips[path] = zipfile.ZipFile(path)
        data = zips[path].read(member)
        gray, size = pagecache.to_gray(data, work_width)
        r = rows_by_id[pid]
        if tuple(size) != (r["width"], r["height"]):
            raise RuntimeError(f"{pid}: stored image is {size}, the reference row says {(r['width'], r['height'])}")
        pagecache.save(out_cache, pid, work_width, gray, size)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--refs-dir", required=True)
    ap.add_argument("--locbulk", required=True, help="directory holding <batch>.tar.bz2")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--limit-batches", type=int, help="smoke runs only: the first N batches")
    a = ap.parse_args(argv)
    os.makedirs(os.path.join(a.out_dir, "alto"), exist_ok=True)
    os.makedirs(os.path.join(a.out_dir, "cache"), exist_ok=True)
    rows = read_refs(a.refs_dir)
    ids = [r["page_id"] for r in rows]
    if len(set(ids)) != len(ids):
        raise SystemExit("page ids are not unique across train and val")
    by_batch = {}
    for r in rows:
        by_batch.setdefault(r["batch"], []).append(r)
    batches = sorted(by_batch)[: a.limit_batches]
    result, jobs = {}, []
    for b in batches:
        tp = os.path.join(a.locbulk, b + ".tar.bz2")
        if not os.path.exists(tp):
            for r in by_batch[b]:
                result[r["page_id"]] = {"status": "batch_not_in_locbulk", "batch": b, "lccn": r["lccn"], "date": r["date"],
                                        "edition": r["edition"], "issue_pages": 0, "within_1pct": 0, "seq": None, "split": r["split"]}
        else:
            jobs.append((b, tp, by_batch[b], os.path.join(a.out_dir, "alto")))
    jobs.sort(key=lambda j: -os.path.getsize(j[1]))  # biggest first
    t0 = time.time()
    print(f"batches 0/{len(jobs)}", flush=True)
    with mp.Pool(a.workers) as pool:
        for i, (b, res) in enumerate(pool.imap_unordered(scan_batch, jobs), 1):
            result.update(res)
            print(f"batches {i}/{len(jobs)} ({b}) {time.time() - t0:.0f}s", flush=True)
    rows_by_id = {r["page_id"]: r for r in rows}
    joined = sorted(p for p, v in result.items() if v["status"] == "joined")
    cache_pages(rows_by_id, joined, os.path.join(a.out_dir, "cache"))
    counts = {}
    for v in result.values():
        counts[v["status"]] = counts.get(v["status"], 0) + 1
    order = sorted(joined, key=sha_key)
    shared = {}
    for p in joined:
        v = result[p]
        k = (v["batch"], v["lccn"], v["date"], v["edition"], v["seq"])
        shared[k] = shared.get(k, 0) + 1
    meta = {"counts": counts, "pages": len(result), "joined": len(joined), "tolerance": TOL,
            "alto_pages_joined_by_several": sum(1 for n in shared.values() if n > 1),
            "selected_for_test_b": order[:300], "wall_seconds": round(time.time() - t0, 1)}
    with open(os.path.join(a.out_dir, "join.json"), "w") as f:
        json.dump({"_meta": meta, "pages": result}, f, sort_keys=True)
    print(json.dumps(meta["counts"], sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
