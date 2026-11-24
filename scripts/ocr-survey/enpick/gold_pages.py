#!/usr/bin/env python3
"""tess_pick.py's --pages for the en-read gold set (tess-loc-gold-v1, #2122): pages with a human transcription of
some boxes, read by Tesseract with the worker's settings (english_pages.tesseract).

  gold_pages.py --en-op-pages pages-en-op.jsonl --en-op-dir DIR --asgold DIR --locbulk DIR --locstats DIR \
      --work DIR [--workers 4]

Overproof dataset 3 (en-op): the 18 pages of en_pick_gold.py's pages file (#1867): LoC's IIIF JPEG and ALTO of each
page, in --en-op-dir by file name, and its crops (box in image pixels, `truth` = the corrected text); each crop is a
region.

en-asgold (American Stories gold, Apache-2.0 text): the pages selection.tsv maps to a Chronicling America page
(ca_page); the image is the gold set's own scan (images/<id>.jpg), LoC's ALTO comes from the bulk archive the
asgold-ca-map.json JP2 URL names (checked in the locstats index, read as english_pages.read_members does). Regions are
regions.jsonl's transcribed `article` regions (box [x0, y0, x1, y1]); headlines and captions are left out, since the
queue is body text.

Beyond Words captions are not used: the plan caps them at a third of the gold set and the two sets above fill it.

Writes WORK/pages.jsonl (with `regions`), WORK/tess/, WORK/alto/ and WORK/gold-report.json.
"""
import argparse
import csv
import json
import multiprocessing as mp
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.normpath(os.path.join(HERE, "..", "..", "..", "edge", "check", "tools"))
sys.path.insert(0, TOOLS if os.path.isdir(TOOLS) else HERE)
import english_pages as ep  # noqa: E402


def archive_from_jp2_url(url):
    m = re.search(r"/batch_([a-z]+_[A-Za-z0-9]+_ver\d\d)/", url)
    return m.group(1) + ".tar.bz2" if m else None


def asgold_pages(asgold, locbulk, locstats, work):
    sel = list(csv.DictReader(open(os.path.join(asgold, "selection.tsv")), delimiter="\t"))
    cmap = {r["file"]: r["ca"] for r in json.load(open(os.path.join(asgold, "asgold-ca-map.json"))) if r.get("ca")}
    by_doc = {c["doc_id"]: c for c in cmap.values()}
    regions = {}
    for r in map(json.loads, open(os.path.join(asgold, "regions.jsonl"))):
        if r["type"] == "article" and r.get("transcribed") and r["text"].strip():
            regions.setdefault(r["id"], []).append({"id": f"{r['id']}/{r['region']}", "box": r["box"], "text": r["text"]})
    out, left = [], {}
    os.makedirs(os.path.join(work, "alto"), exist_ok=True)
    for row in sel:
        d = row.get("ca_page")
        if not d:
            continue
        ca = by_doc.get(d)
        archive = archive_from_jp2_url(ca["jp2"]) if ca else None
        if not archive or not ep.in_locstats(locstats, archive, [d]):
            left[d] = "not_in_locstats"
            continue
        if not os.path.exists(os.path.join(work, "alto", d + ".xml")):
            if d not in ep.read_members(os.path.join(locbulk, archive), {d}, os.path.join(work, "alto")):
                left[d] = "not_in_archive"
                continue
        m = ep.DOC.match(d)
        out.append({"page": "%s/%s/ed-%d/seq-%d" % (m["lccn"], m["date"], int(m["ed"]), int(m["seq"])), "page_id": d,
                    "source": ep.source_url(d), "date": m["date"], "label": "%s, %s, page %d" % (m["lccn"], m["date"], int(m["seq"])),
                    "alto": os.path.join("alto", d + ".xml"), "image": os.path.abspath(os.path.join(asgold, "images", row["id"] + ".jpg")),
                    "regions": regions.get(row["id"], []), "gold_source": "en-asgold"})
    return out, left


def en_op_pages(path, en_op_dir):
    out = []
    for p in map(json.loads, open(path)):
        out.append({"page": p["page"], "page_id": p["page"].replace("/", "_"), "source": p.get("source"), "date": p["date"],
                    "label": p.get("label"), "alto": os.path.abspath(os.path.join(en_op_dir, os.path.basename(p["alto"]))),
                    "image": os.path.abspath(os.path.join(en_op_dir, os.path.basename(p["image"]))),
                    "regions": [{"id": c["id"], "box": c["box"], "text": c["truth"]} for c in p["crops"] if c.get("truth")],
                    "gold_source": "overproof"})
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--en-op-pages", required=True)
    ap.add_argument("--en-op-dir", required=True)
    ap.add_argument("--asgold", required=True)
    ap.add_argument("--locbulk", required=True)
    ap.add_argument("--locstats", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--workers", type=int, default=4)
    a = ap.parse_args(argv)
    os.makedirs(os.path.join(a.work, "tess"), exist_ok=True)
    pages = en_op_pages(a.en_op_pages, a.en_op_dir)
    ag, left = asgold_pages(a.asgold, a.locbulk, a.locstats, a.work)
    pages += ag
    tess_dir = os.path.join(a.work, "tess")
    jobs = []
    for p in pages:
        p["tsv"] = os.path.join("tess", p["page_id"] + ".tsv")
        if ep.done_before(tess_dir, p["page_id"], p["image"]) != 0:
            jobs.append((p["page_id"], p["image"], tess_dir))
    with mp.Pool(a.workers) as pool:
        for d, code, err in pool.imap_unordered(ep.tesseract, jobs):
            print(f"tesseract {d} rc={code}", flush=True)
            if code != 0:
                left[d] = "tesseract_failed: %s" % err[-200:]
    meta = ep.tess_meta()
    with open(os.path.join(a.work, "pages.jsonl"), "w") as f:
        for p in pages:
            if p["page_id"] not in left and p["regions"]:
                f.write(json.dumps(p) + "\n")
    rep = {"pages": len(pages), "left_out": left, "written": sum(1 for p in pages if p["page_id"] not in left and p["regions"]),
           "regions": {s: sum(len(p["regions"]) for p in pages if p["gold_source"] == s) for s in ("overproof", "en-asgold")},
           "tesseract": meta}
    json.dump(rep, open(os.path.join(a.work, "gold-report.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(rep, indent=1))


if __name__ == "__main__":
    main()
