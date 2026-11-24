#!/usr/bin/env python3
"""LoC ALTO for Beyond Words training pages, from the bulk batch archives (#2107 combined segmenter).

  --stage map      Beyond Words' source_url gives (lccn, reel, folder, file); the scan index (index-full.jsonl, as
                   bwfalse.bw_map reads it) gives the Chronicling America doc id <lccn>_<date>_ed-N_seq-N. A page
                   with no doc id, or with more than one, gets no ALTO (counted).
  --stage extract  The locstats index names each doc's archive; each archive is streamed once, stopping at its last
                   wanted page (bwcap.py's method, #2122). Writes <alto-dir>/<doc id>.xml with a sidecar (archive,
                   member, sha256), then alto-map.json for wordboxes.py --alto-map (page id -> alto, scan size).
Pages without ALTO keep NaN word features; they are counted, never filled from another source.
Data: biglam/loc_beyond_words (CC0); Library of Congress, Chronicling America (ALTO, scans).

    alto_bulk.py --stage map --rows BW_TRAIN.jsonl --ids ids.txt --index index-full.jsonl --out doc-map.json
    alto_bulk.py --stage extract --doc-map doc-map.json --scan-sizes scan-sizes.json --locstats-dir D
                 --locbulk-dir D --alto-dir D --out alto-map.json [--workers 3]
"""
import argparse
import collections
import glob
import hashlib
import json
import multiprocessing as mp
import os
import sys
import tarfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_diag"))
import bwfalse  # noqa: E402


def member_doc(name):
    """doc id of a member <lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml, else None."""
    p = name.split("/")
    if len(p) != 7 or p[6] != "ocr.xml" or not p[4].startswith("ed-") or not p[5].startswith("seq-"):
        return None
    try:
        return "%s_%s-%s-%s_ed-%d_seq-%d" % (p[0], p[1], p[2], p[3], int(p[4][3:]), int(p[5][4:]))
    except ValueError:
        return None


def stage_map(a):
    ids = [ln.strip() for ln in open(a.ids) if ln.strip()]
    want = set(ids)
    tails = {}
    for r in bwfalse.read_rows(a.rows):
        if str(r["page_id"]) in want:
            if r.get("split") != "train":
                raise SystemExit("not a training row: %s" % r["page_id"])
            u = bwfalse.lit(r["meta"]).get("source_url") or ""
            if "/data/" in u:
                tails[str(r["page_id"])] = bwfalse.path_tail(u)
    by_tail = collections.defaultdict(set)
    need = set(tails.values())
    with open(a.index) as f:
        for ln in f:
            r = json.loads(ln)
            if not bwfalse.DOC.match(r.get("doc_id", "")):
                continue
            t = bwfalse.iiif_tail(r["iiif_base"])
            if t in need:
                by_tail[t].add(r["doc_id"])
    out, st = {}, collections.Counter()
    for pid in ids:
        docs = sorted(by_tail.get(tails.get(pid), ()))
        st["one doc" if len(docs) == 1 else "no doc" if not docs else "several docs"] += 1
        if len(docs) == 1:
            out[pid] = docs[0]
    json.dump({"counts": dict(st), "docs": out}, open(a.out, "w"), indent=1, sort_keys=True)
    print(json.dumps(dict(st), sort_keys=True))
    return 0


def _archive_job(job):
    tar_path, wanted, alto_dir = job
    want = {d for d in wanted if not os.path.exists(os.path.join(alto_dir, d + ".xml"))}
    got = len(wanted) - len(want)
    if want:
        with tarfile.open(tar_path, "r|bz2") as tf:
            for m in tf:
                d = member_doc(m.name)
                if d not in want:
                    continue
                data = tf.extractfile(m).read()
                p = os.path.join(alto_dir, d + ".xml")
                with open(p + ".part", "wb") as f:
                    f.write(data)
                os.replace(p + ".part", p)
                with open(p + ".json", "w") as f:
                    json.dump({"archive": os.path.basename(tar_path), "member": m.name,
                               "sha256": hashlib.sha256(data).hexdigest()}, f, sort_keys=True)
                want.discard(d)
                got += 1
                if not want:
                    break
    return os.path.basename(tar_path), got, sorted(want)


def stage_extract(a):
    import pyarrow.parquet as pq
    dm = json.load(open(a.doc_map))["docs"]
    sizes = json.load(open(a.scan_sizes))
    os.makedirs(a.alto_dir, exist_ok=True)
    docs = {d for p, d in dm.items() if p in sizes}
    by_lccn = collections.defaultdict(set)
    for d in docs:
        by_lccn[d.split("_")[0]].add(d)
    arch = collections.defaultdict(set)
    for pth in sorted(glob.glob(os.path.join(a.locstats_dir, "*.parquet"))):
        t = pq.read_table(pth, columns=["doc_id", "archive"])
        for d, ar in zip(t.column("doc_id").to_pylist(), t.column("archive").to_pylist()):
            if d in docs:
                arch[d].add(ar)
    jobs = collections.defaultdict(list)
    for d in sorted(docs):
        if arch.get(d):
            jobs[os.path.join(a.locbulk_dir, sorted(arch[d])[-1])].append(d)
    not_indexed = sorted(docs - set(arch))
    print("docs %d, in locstats %d, archives %d" % (len(docs), len(docs) - len(not_indexed), len(jobs)), flush=True)
    not_found = []
    with mp.Pool(a.workers) as pool:
        for n, (name, got, left) in enumerate(pool.imap_unordered(_archive_job, [(k, v, a.alto_dir) for k, v in sorted(jobs.items())]), 1):
            not_found += left
            print("archives %d/%d (%s: %d, not found %d)" % (n, len(jobs), name, got, len(left)), flush=True)
    amap = {}
    for p, d in sorted(dm.items()):
        x = os.path.join(a.alto_dir, d + ".xml")
        if p in sizes and os.path.exists(x):
            amap[p] = {"alto": x, "width": sizes[p]["width"], "height": sizes[p]["height"]}
    json.dump(amap, open(a.out, "w"), indent=1, sort_keys=True)
    summ = {"pages_with_scan": len(sizes), "pages_with_doc": len(docs), "not_in_locstats": len(not_indexed),
            "not_found_in_archive": len(not_found), "pages_with_alto": len(amap)}
    json.dump(summ, open(a.out + ".summary.json", "w"), indent=1, sort_keys=True)
    print(json.dumps(summ, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--stage", required=True, choices=("map", "extract"))
    ap.add_argument("--rows")
    ap.add_argument("--ids")
    ap.add_argument("--index")
    ap.add_argument("--doc-map")
    ap.add_argument("--scan-sizes")
    ap.add_argument("--locstats-dir")
    ap.add_argument("--locbulk-dir")
    ap.add_argument("--alto-dir")
    ap.add_argument("--out", required=True)
    ap.add_argument("--workers", type=int, default=3)
    a = ap.parse_args(argv)
    return stage_map(a) if a.stage == "map" else stage_extract(a)


if __name__ == "__main__":
    sys.exit(main())
