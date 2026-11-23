#!/usr/bin/env python3
"""Join Beyond Words pages to LoC ALTO pages in the local `locbulk` copy (#2107 step 2, test B).

Method sources: step 1 of #2107 (#2112) and its reference rows (#2113); #2103; Beyond Words (Library of
Congress, CC0, biglam/loc_beyond_words@6c7f5fb3, Lee et al., arXiv:2005.01583); American Stories
(Dell et al., arXiv:2308.12477).

    join_bw.py --refs-dir DIR --locbulk DIR --out-dir DIR [--workers 6] [--weights 7,12,12,12 --shard-index I]
    join_bw.py --refs-dir DIR --out-dir DIR --finish

The scan can be sharded over containers (batches dealt by size, per-shard weights); each shard writes
join.shard<I>.json and the ALTO of its joined pages. `--finish`, on the container that has the images, merges the
shard files (the shards' alto/ files copied in), caches the joined pages' images, and reports how many pages in plan
order were tried to reach 300 joins and why the rest failed.

The plan's join: a Beyond Words page joins when exactly one ALTO page of the same LCCN, date and edition has an
aspect ratio (ALTO Page WIDTH / HEIGHT) within 1% of the Beyond Words image's (|r_alto / r_image - 1| <= 0.01).
Statuses: joined | ambiguous (two or more within 1%) | no_aspect_match | issue_not_in_tar | batch_not_in_locbulk.

Reading rule: each page's Chronicling America identity (`meta.ca_identity`) names its batch, and the issue
is looked up inside that batch's `<batch>.tar.bz2` only; a locbulk-wide search would mean reading all 2.3 TB.
Members are `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<k>/ocr.xml`. The tars are read as a stream and never
modified. Members of one issue need not be adjacent.

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
    """One tar: returns (batch, {page id: record}). Members of an issue need not be contiguous: the dimensions of
    every wanted issue's pages are kept, the bytes of pages within 1% of a wanted image's aspect are spilled to a
    temporary directory beside the output, and the decision is made when the stream ends."""
    import shutil
    batch, tar_path, pages, out_alto = args
    wanted = {}
    for r in pages:
        wanted.setdefault((r["lccn"], r["date"], r["edition"]), []).append(r)
    spill = os.path.join(os.path.dirname(out_alto.rstrip("/")), "spill-" + batch)
    shutil.rmtree(spill, ignore_errors=True)
    os.makedirs(spill)
    dims, names = {}, {}
    with tarfile.open(tar_path, "r|bz2") as tf:
        for m in tf:
            pm = parse_member(m.name)
            if pm is None or pm[0] not in wanted:
                continue
            key, seq = pm
            data = tf.extractfile(m).read()
            d = altolayout.page_dims(data)
            if not d:
                continue
            dims.setdefault(key, {})[seq] = (d[0], d[1])
            if any(decide(r["width"], r["height"], {seq: d})[0] == "joined" for r in wanted[key]):
                fn = os.path.join(spill, f"{len(names)}.xml")
                with open(fn, "wb") as f:
                    f.write(data)
                names[(key, seq)] = fn
    res = {}
    for key, rs in wanted.items():
        for r in rs:
            status, seq, n = decide(r["width"], r["height"], dims.get(key, {}))
            res[r["page_id"]] = {"status": status, "batch": batch, "lccn": r["lccn"], "date": r["date"],
                                 "edition": r["edition"], "issue_pages": len(dims.get(key, {})), "within_1pct": n,
                                 "seq": seq, "split": r["split"]}
            if status == "joined":
                shutil.copyfile(names[(key, seq)], os.path.join(out_alto, r["page_id"] + ".xml"))
    shutil.rmtree(spill, ignore_errors=True)
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


def split_jobs(jobs, weights):
    """Deal jobs (biggest first) to shards, each going to the shard with the least size per unit of weight."""
    loads = [0.0] * len(weights)
    out = [[] for _ in weights]
    for j in sorted(jobs, key=lambda j: -os.path.getsize(j[1])):
        k = min(range(len(weights)), key=lambda i: loads[i] / weights[i])
        out[k].append(j)
        loads[k] += os.path.getsize(j[1])
    return out


def finish(out_dir, rows):
    """Merge join.shard*.json, cache the joined pages' images, write join.json."""
    t0 = time.time()
    result, scan_secs = {}, []
    for fn in sorted(os.listdir(out_dir)):
        if fn.startswith("join.shard") and fn.endswith(".json"):
            d = json.load(open(os.path.join(out_dir, fn)))
            result.update(d["pages"])
            scan_secs.append(d["wall_seconds"])
    if len(result) != len(rows):
        raise SystemExit(f"shards cover {len(result)} of {len(rows)} pages")
    rows_by_id = {r["page_id"]: r for r in rows}
    joined = sorted(p for p, v in result.items() if v["status"] == "joined")
    missing = [p for p in joined if not os.path.exists(os.path.join(out_dir, "alto", p + ".xml"))]
    if missing:
        raise SystemExit(f"{len(missing)} joined pages have no ALTO file here (copy the shards' alto/ files in)")
    os.makedirs(os.path.join(out_dir, "cache"), exist_ok=True)
    cache_pages(rows_by_id, joined, os.path.join(out_dir, "cache"))
    counts = {}
    for v in result.values():
        counts[v["status"]] = counts.get(v["status"], 0) + 1
    shared = {}
    for p in joined:
        v = result[p]
        k = (v["batch"], v["lccn"], v["date"], v["edition"], v["seq"])
        shared[k] = shared.get(k, 0) + 1
    order = sorted(joined, key=sha_key)
    # walk in plan order until 300 have joined: how many pages were tried and why the others failed
    tried, failed, njoined = 0, {}, 0
    for p in sorted(result, key=sha_key):
        tried += 1
        st = result[p]["status"]
        if st == "joined":
            njoined += 1
        else:
            failed[st] = failed.get(st, 0) + 1
        if njoined >= 300:
            break
    meta = {"counts": counts, "pages": len(result), "joined": len(joined), "tolerance": TOL,
            "alto_pages_joined_by_several": sum(1 for n in shared.values() if n > 1),
            "selected_for_test_b": order[:300], "pages_tried_to_reach_300": tried, "failures_before_300": failed,
            "scan_wall_seconds_per_shard": scan_secs, "finish_wall_seconds": round(time.time() - t0, 1)}
    with open(os.path.join(out_dir, "join.json"), "w") as f:
        json.dump({"_meta": meta, "pages": result}, f, sort_keys=True)
    print(json.dumps(meta["counts"], sort_keys=True))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--refs-dir", required=True)
    ap.add_argument("--locbulk", help="directory holding <batch>.tar.bz2 (scan)")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--weights", default="1", help="comma list of shard capacities, e.g. 7,12,12,12")
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--finish", action="store_true", help="merge join.shard*.json in --out-dir, cache images, write join.json")
    ap.add_argument("--limit-batches", type=int, help="smoke runs only: the first N batches")
    a = ap.parse_args(argv)
    rows = read_refs(a.refs_dir)
    ids = [r["page_id"] for r in rows]
    if len(set(ids)) != len(ids):
        raise SystemExit("page ids are not unique across train and val")
    if a.finish:
        finish(a.out_dir, rows)
        return 0
    os.makedirs(os.path.join(a.out_dir, "alto"), exist_ok=True)
    by_batch = {}
    for r in rows:
        by_batch.setdefault(r["batch"], []).append(r)
    batches = sorted(by_batch)[: a.limit_batches]
    result, jobs = {}, []
    weights = [float(w) for w in a.weights.split(",")]
    for b in batches:
        tp = os.path.join(a.locbulk, b + ".tar.bz2")
        if not os.path.exists(tp):
            if a.shard_index == 0:
                for r in by_batch[b]:
                    result[r["page_id"]] = {"status": "batch_not_in_locbulk", "batch": b, "lccn": r["lccn"], "date": r["date"],
                                            "edition": r["edition"], "issue_pages": 0, "within_1pct": 0, "seq": None, "split": r["split"]}
        else:
            jobs.append((b, tp, by_batch[b], os.path.join(a.out_dir, "alto")))
    jobs = split_jobs(jobs, weights)[a.shard_index]
    t0 = time.time()
    print(f"batches 0/{len(jobs)} joined 0", flush=True)
    joined = 0
    with mp.Pool(a.workers) as pool:
        for i, (b, res) in enumerate(pool.imap_unordered(scan_batch, jobs), 1):
            result.update(res)
            joined += sum(1 for v in res.values() if v["status"] == "joined")
            print(f"batches {i}/{len(jobs)} joined {joined} ({b}) {time.time() - t0:.0f}s", flush=True)
    with open(os.path.join(a.out_dir, f"join.shard{a.shard_index}.json"), "w") as f:
        json.dump({"pages": result, "wall_seconds": round(time.time() - t0, 1)}, f, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
