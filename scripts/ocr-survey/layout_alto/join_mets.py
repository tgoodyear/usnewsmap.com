#!/usr/bin/env python3
"""Exact Beyond Words -> LoC ALTO join through the NDNP issue METS (#2107 step 2, test B robustness rerun).

Method sources: step 1 of #2107 (#2112) and its references (#2113); #2103; Beyond Words (Library of Congress, CC0,
biglam/loc_beyond_words@6c7f5fb3, Lee et al., arXiv:2005.01583); American Stories (Dell et al., arXiv:2308.12477);
LoC NDNP issue METS (the batches' `mets/<lccn>/<YYYYMMDDEE>.xml`, as held for #2115).

    join_mets.py --mode mets  --refs-dir DIR --mets-dir DIR --out FILE [--limit 300]
    join_mets.py --mode manifest --join METS_JOIN.json --man-dir DIR --out FILE   (secondary; see manifest_join)
    join_mets.py --mode fetch --join FILE --locbulk DIR --out-dir DIR [--workers 12] [--shard-index I --shard-count N]
    join_mets.py --mode finish --join FILE --out-dir DIR

mets: a Beyond Words page matches the METS page of the same LCCN, date and edition (the issue file
`<batch>/mets/<lccn>/<YYYYMMDD><EE>.xml`, EE the two-digit edition, in the batch named by the page's
Chronicling America identity) whose MODS `reel number` equals Beyond Words' reel and whose `reel sequence number`
equals its frame. The page's 1-based position among the structMap's `np:page` divs is the `seq-N` of the locbulk
tar, so its ALTO is `<lccn>/YYYY/MM/DD/ed-E/seq-N/ocr.xml`. Statuses: joined | no_mets_batch | no_mets_issue |
no_page_match | multiple_match. `selected` is the first --limit joined pages in sha256("2107:s2:"+page id) order.
fetch: streams the batch tars (never modified) and writes the selected pages' ALTO, with the issue's `seq-*` count
and the ALTO page size. finish: merges shards, checks each page's METS page count and ALTO file name against the tar,
caches the images (pagecache format) and writes the page table.
"""
import argparse
import hashlib
import json
import multiprocessing as mp
import os
import sys
import tarfile
import time
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import altolayout  # noqa: E402


def sha_key(page_id):
    return hashlib.sha256(("2107:s2:" + page_id).encode()).hexdigest()


def _l(tag):
    return tag.rsplit("}", 1)[-1]


def mets_pages(path):
    """[{reel, reel_seq, ocr}] in structMap page-div order. ocr: the href of the page's USE="ocr" file."""
    root = ET.parse(path).getroot()
    mods = {}
    for sec in root.iter():
        if _l(sec.tag) == "dmdSec":
            ids = {}
            for e in sec.iter():
                if _l(e.tag) == "identifier" and e.get("type") in ("reel number", "reel sequence number"):
                    ids[e.get("type")] = (e.text or "").strip()
            mods[sec.get("ID")] = ids
    files = {}
    for f in root.iter():
        if _l(f.tag) == "file" and f.get("USE") == "ocr":
            for c in f:
                if _l(c.tag) == "FLocat":
                    files[f.get("ID")] = c.get("{http://www.w3.org/1999/xlink}href")
    pages = []
    for d in root.iter():
        if _l(d.tag) == "div" and d.get("TYPE") == "np:page":
            ocr = None
            for fp in d:
                if _l(fp.tag) == "fptr" and fp.get("FILEID") in files:
                    ocr = files[fp.get("FILEID")]
            m = mods.get(d.get("DMDID"), {})
            pages.append({"reel": m.get("reel number"), "reel_seq": m.get("reel sequence number"), "ocr": ocr})
    return pages


def mets_join(refs_dir, mets_dir, limit):
    rows = []
    for split in ("train", "val"):
        with open(os.path.join(refs_dir, f"biglam__loc_beyond_words__{split}.jsonl")) as f:
            rows += [json.loads(ln) for ln in f]
    res, cache = {}, {}
    for r in rows:
        ci = r["meta"]["ca_identity"]
        pid = r["page_id"]
        rec = {"batch": ci["batch"], "lccn": ci["lccn"], "date": ci["date"], "edition": int(ci["edition"]),
               "reel": ci["reel"], "frame": int(ci["frame"]), "split": r["split"], "width": r["width"], "height": r["height"],
               "status": None, "seq": None, "mets_pages": None, "ocr": None}
        res[pid] = rec
        if not os.path.isdir(os.path.join(mets_dir, ci["batch"], "mets")):
            rec["status"] = "no_mets_batch"
            continue
        y, m, d = ci["date"].split("-")
        p = os.path.join(mets_dir, ci["batch"], "mets", ci["lccn"], f"{y}{m}{d}{int(ci['edition']):02d}.xml")
        if not os.path.exists(p):
            rec["status"] = "no_mets_issue"
            continue
        if p not in cache:
            cache[p] = mets_pages(p)
        pages = cache[p]
        hits = [i for i, pg in enumerate(pages, 1)
                if pg["reel"] == ci["reel"] and pg["reel_seq"] is not None and pg["reel_seq"].isdigit() and int(pg["reel_seq"]) == int(ci["frame"])]
        rec["mets_pages"] = len(pages)
        if len(hits) == 1:
            rec.update(status="joined", seq=hits[0], ocr=pages[hits[0] - 1]["ocr"])
        else:
            rec["status"] = "no_page_match" if not hits else "multiple_match"
    joined = sorted((p for p, v in res.items() if v["status"] == "joined"), key=sha_key)
    counts = {}
    for v in res.values():
        counts[v["status"]] = counts.get(v["status"], 0) + 1
    return {"pages": res, "_meta": {"counts": counts, "joined": len(joined), "selected": joined[:limit], "limit": limit}}


def manifest_join(mets_join_json, man_dir, limit):
    """SECONDARY, not the pre-registered join. For batches whose held METS covers only some issues, the batch's file
    manifest (`data/<lccn>/<reel>/<YYYYMMDDEE>/<NNNN>.xml`) lists every issue's OCR files. A Beyond Words page matches the
    manifest file in its issue and reel whose number is its frame plus a per-batch offset, taken from the pages that
    the METS join (same batch) resolved: the offset must be the same for all of them (else the batch is skipped).
    The page's seq-N is the file's 1-based rank among the issue's NNNN.xml files."""
    pages = json.load(open(mets_join_json))["pages"]
    offs = {}
    for v in pages.values():
        if v["status"] == "joined":
            name = (v["ocr"] or "").rsplit("/", 1)[-1][:-4]
            if name.isdigit():
                offs.setdefault(v["batch"], set()).add(int(name) - v["frame"])
    issues = {}
    for fn in os.listdir(man_dir):
        b = fn[:-4]
        for ln in open(os.path.join(man_dir, fn)):
            p = ln.strip().split("/")
            if len(p) == 5 and p[4].endswith(".xml") and p[4][:-4].isdigit():
                issues.setdefault((b, p[1], p[3]), []).append((p[2], p[4][:-4]))
    res = {}
    for pid, v in pages.items():
        rec = dict(v, status=None, seq=None, mets_pages=None, ocr=None)
        res[pid] = rec
        o = offs.get(v["batch"])
        if v["status"] == "no_mets_batch":
            rec["status"] = "no_manifest_batch"
            continue
        if not o or len(o) != 1:
            rec["status"] = "no_offset_for_batch"
            continue
        y, m, d = v["date"].split("-")
        files = issues.get((v["batch"], v["lccn"], f"{y}{m}{d}{int(v['edition']):02d}"))
        if not files:
            rec["status"] = "no_manifest_issue"
            continue
        want = f"{v['frame'] + next(iter(o)):04d}"
        files = sorted(files, key=lambda t: t[1])
        hits = [i for i, (reel, nm) in enumerate(files, 1) if reel == v["reel"] and nm == want]
        rec["mets_pages"] = len(files)  # here: the manifest's page count
        if len(hits) == 1:
            rec.update(status="joined", seq=hits[0], ocr=f"{want}.xml")
        else:
            rec["status"] = "no_page_match" if not hits else "multiple_match"
    joined = sorted((p for p, v in res.items() if v["status"] == "joined"), key=sha_key)
    counts = {}
    for v in res.values():
        counts[v["status"]] = counts.get(v["status"], 0) + 1
    return {"pages": res, "_meta": {"counts": counts, "joined": len(joined), "selected": joined[:limit], "limit": limit,
                                    "batch_offsets": {b: sorted(o) for b, o in offs.items()}, "kind": "manifest-extended (secondary)"}}


# ---------------------------------------------------------------- fetch

def member_name(v):
    y, m, d = v["date"].split("-")
    return f"{v['lccn']}/{y}/{m}/{d}/ed-{v['edition']}/seq-{v['seq']}/ocr.xml"


def fetch_batch(args):
    batch, tar_path, wanted, out_alto = args  # wanted: {page id: record}
    by_name = {member_name(v): pid for pid, v in wanted.items()}
    issues = {}
    for v in wanted.values():
        y, m, d = v["date"].split("-")
        issues[f"{v['lccn']}/{y}/{m}/{d}/ed-{v['edition']}/"] = 0
    out = {}
    with tarfile.open(tar_path, "r|bz2") as tf:
        for m in tf:
            if not m.name.endswith("/ocr.xml"):
                continue
            pre = m.name.rsplit("seq-", 1)[0]
            if pre in issues:
                issues[pre] += 1
            pid = by_name.get(m.name)
            if pid:
                data = tf.extractfile(m).read()
                with open(os.path.join(out_alto, pid + ".xml"), "wb") as f:
                    f.write(data)
                out[pid] = {"alto_size": altolayout.page_dims(data)}
    for pid, v in wanted.items():
        y, m, d = v["date"].split("-")
        rec = out.setdefault(pid, {"alto_size": None})
        rec["tar_issue_pages"] = issues[f"{v['lccn']}/{y}/{m}/{d}/ed-{v['edition']}/"]
        rec["found"] = rec["alto_size"] is not None
    return batch, out


def do_fetch(a):
    j = json.load(open(a.join))
    sel = j["_meta"]["selected"]
    by_batch = {}
    for pid in sel:
        by_batch.setdefault(j["pages"][pid]["batch"], {})[pid] = j["pages"][pid]
    jobs = [(b, os.path.join(a.locbulk, b + ".tar.bz2"), w, os.path.join(a.out_dir, "alto")) for b, w in sorted(by_batch.items())]
    jobs.sort(key=lambda t: -os.path.getsize(t[1]))
    mine = [t for k, t in enumerate(jobs) if k % a.shard_count == a.shard_index]
    os.makedirs(os.path.join(a.out_dir, "alto"), exist_ok=True)
    res, t0 = {}, time.time()
    print(f"batches 0/{len(mine)} pages 0", flush=True)
    with mp.Pool(a.workers) as pool:
        for i, (b, o) in enumerate(pool.imap_unordered(fetch_batch, mine), 1):
            res.update(o)
            print(f"batches {i}/{len(mine)} pages {len(res)} ({b}) {time.time() - t0:.0f}s", flush=True)
    with open(os.path.join(a.out_dir, f"fetch.shard{a.shard_index}.json"), "w") as f:
        json.dump({"pages": res, "wall_seconds": round(time.time() - t0, 1)}, f, sort_keys=True)


def do_finish(a):
    import join_bw
    j = json.load(open(a.join))
    got, secs = {}, []
    for fn in sorted(os.listdir(a.out_dir)):
        if fn.startswith("fetch.shard"):
            d = json.load(open(os.path.join(a.out_dir, fn)))
            got.update(d["pages"])
            secs.append(d["wall_seconds"])
    sel = j["_meta"]["selected"]
    out, chk = {}, {"found": 0, "not_found": 0, "mets_pages_equal_tar_pages": 0, "mets_pages_differ": 0,
                    "ocr_name_matches_reel_seq": 0, "ocr_name_differs": 0, "aspect_within_1pct": 0, "aspect_off": 0}
    for pid in sel:
        v, g = dict(j["pages"][pid]), got.get(pid)
        if not g or not g["found"]:
            v["status"] = "alto_not_in_tar"
            chk["not_found"] += 1
        else:
            chk["found"] += 1
            chk["mets_pages_equal_tar_pages" if g["tar_issue_pages"] == v["mets_pages"] else "mets_pages_differ"] += 1
            name = (v["ocr"] or "").rsplit("/", 1)[-1]
            chk["ocr_name_matches_reel_seq" if name == f"{v['frame']:04d}.xml" else "ocr_name_differs"] += 1
            w, h = g["alto_size"]
            ok = abs(w * v["height"] - v["width"] * h) * 100 <= v["width"] * h
            chk["aspect_within_1pct" if ok else "aspect_off"] += 1
            v["tar_issue_pages"], v["alto_size"] = g["tar_issue_pages"], g["alto_size"]
        out[pid] = v
    ids = [p for p in sel if out[p]["status"] == "joined"]
    refs = {}
    for split in ("train", "val"):
        for ln in open(os.path.join(a.refs_dir, f"biglam__loc_beyond_words__{split}.jsonl")):
            r = json.loads(ln)
            if r["page_id"] in ids:
                refs[r["page_id"]] = {"width": r["width"], "height": r["height"], "image_path": r["image_path"]}
    os.makedirs(os.path.join(a.out_dir, "cache"), exist_ok=True)
    join_bw.cache_pages(refs, ids, os.path.join(a.out_dir, "cache"))
    meta = dict(j["_meta"], checks=chk, fetch_wall_seconds_per_shard=secs)
    with open(os.path.join(a.out_dir, "join.json"), "w") as f:
        json.dump({"_meta": meta, "pages": out}, f, sort_keys=True)
    print(json.dumps(chk, sort_keys=True))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--mode", required=True, choices=["mets", "manifest", "fetch", "finish"])
    ap.add_argument("--refs-dir")
    ap.add_argument("--mets-dir")
    ap.add_argument("--join")
    ap.add_argument("--man-dir", help="manifest mode: per-batch lists of `data/...xml` paths")
    ap.add_argument("--out")
    ap.add_argument("--out-dir")
    ap.add_argument("--locbulk")
    ap.add_argument("--limit", type=int, default=300)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    a = ap.parse_args(argv)
    if a.mode == "mets":
        res = mets_join(a.refs_dir, a.mets_dir, a.limit)
        with open(a.out, "w") as f:
            json.dump(res, f, sort_keys=True)
        print(json.dumps(res["_meta"]["counts"], sort_keys=True), "selected", len(res["_meta"]["selected"]))
    elif a.mode == "manifest":
        res = manifest_join(a.join, a.man_dir, a.limit)
        with open(a.out, "w") as f:
            json.dump(res, f, sort_keys=True)
        print(json.dumps(res["_meta"]["counts"], sort_keys=True), "selected", len(res["_meta"]["selected"]))
    elif a.mode == "fetch":
        do_fetch(a)
    else:
        if not a.refs_dir:
            raise SystemExit("--refs-dir is needed for finish")
        do_finish(a)
    return 0


if __name__ == "__main__":
    sys.exit(main())
