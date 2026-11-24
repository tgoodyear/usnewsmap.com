#!/usr/bin/env python3
"""Volunteer gutter labels (check site language en-gutter) as line labels for the combined segmenter (#2107).

Reads the public CC0 export (GET https://check.usnewsmap.com/api/export?lang=en-gutter, JSON lines, cached
five minutes by the site; one request per run), keeps the settled tasks of one set (default en2107-l2) whose
outcome is yes (`a`) or no (`b`), and maps each task to its segment through the set's build manifest
(tasks.jsonl: task id -> private.segment_id) and the segment file (segments-l2.jsonl: page id, image path and the
refitted line x at the top and bottom, y0, y1, in the full-resolution scan's pixels). Unsure, disputed, held,
withdrawn and unsettled tasks are counted, never labelled. Where the export carries segment_id (settled tasks),
it must equal the manifest's.

Output, one JSON line per labelled segment: {page_id, segment_id, task_id, role, method, image_path, x_top,
x_bottom, y0, y1, label (1 yes, 0 no), counts}; build_features.py --line-labels reads it.

--exclude-method drops segments by method. The default drops the bw-nontext gold negatives: they lie on Beyond
Words validation pages, which the English false-gutter measure uses.

    export_engutter.py --tasks tasks.jsonl --segments segments-l2.jsonl --out labels.jsonl
                       [--export-file check-en-gutter.jsonl | --export-url URL] [--save-export FILE]
                       [--set en2107-l2] [--exclude-method bw-nontext ...] [--summary summary.json]
                       [--pages-file engutter-train-pages.txt]

Training reads only the train half (--pages-file engutter-train-pages.txt). The English rule is evaluated only
once the test half has at least 300 settled non-gold lines (summary counts.labelled_non_gold with the test pages).
"""
import argparse
import collections
import json
import sys
import urllib.request

URL = "https://check.usnewsmap.com/api/export?lang=en-gutter"
UA = "research-client/1.0"


def read_jsonl(text):
    return [json.loads(ln) for ln in text.splitlines() if ln.strip()]


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("utf-8")


GOLD_ROLES = {"gold-positive", "gold-negative"}


def build(export, tasks, segments, set_name, exclude_methods, pages=None):
    seg_of = {t["id"]: t["private"]["segment_id"] for t in tasks if t.get("set") == set_name}
    segs = {s["segment_id"]: s for s in segments}
    c = collections.Counter()
    out = []
    for r in export:
        if r.get("kind") != "gutter" or r.get("set") != set_name:
            continue
        c["tasks"] += 1
        if r.get("held") or r.get("outcome_withdrawn"):
            c["held_or_withdrawn"] += 1
            continue
        if not r.get("settled"):
            c["unsettled"] += 1
            continue
        oc = r.get("outcome")
        if oc not in ("a", "b"):
            c["settled_" + str(oc)] += 1
            continue
        sid = seg_of.get(r["id"])
        if sid is None:
            raise SystemExit("task %s is not in the manifest" % r["id"])
        if r.get("segment_id") is not None and r["segment_id"] != sid:
            raise SystemExit("task %s: export segment %s, manifest %s" % (r["id"], r["segment_id"], sid))
        s = segs.get(sid)
        if s is None:
            c["segment_missing"] += 1
            continue
        if pages is not None and s["page_id"] not in pages:
            c["other_half_page"] += 1
            continue
        if s.get("method") in exclude_methods:
            c["excluded_method_" + s["method"]] += 1
            continue
        c["labelled_" + ("yes" if oc == "a" else "no")] += 1
        if s.get("role") not in GOLD_ROLES:
            c["labelled_non_gold"] += 1
        out.append({"page_id": s["page_id"], "segment_id": sid, "task_id": r["id"], "role": s.get("role"),
                    "method": s.get("method"), "image_path": s.get("image_path"), "x_top": s["x"],
                    "x_bottom": s.get("x_bottom", s["x"]), "y0": s["y0"], "y1": s["y1"], "label": 1 if oc == "a" else 0,
                    "counts": r.get("counts")})
    return out, dict(sorted(c.items()))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--tasks", required=True)
    ap.add_argument("--segments", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--export-file")
    ap.add_argument("--export-url", default=URL)
    ap.add_argument("--save-export")
    ap.add_argument("--set", default="en2107-l2")
    ap.add_argument("--exclude-method", action="append", default=None)
    ap.add_argument("--summary")
    ap.add_argument("--pages-file", help="keep only segments on these pages (train_inventory.py's engutter-*-pages.txt)")
    a = ap.parse_args(argv)
    excl = set(a.exclude_method if a.exclude_method is not None else ["bw-nontext"])
    text = open(a.export_file).read() if a.export_file else fetch(a.export_url)
    if a.save_export:
        open(a.save_export, "w").write(text)
    tasks = read_jsonl(open(a.tasks).read())
    segments = read_jsonl(open(a.segments).read())
    pages = {ln.strip() for ln in open(a.pages_file) if ln.strip()} if a.pages_file else None
    out, c = build(read_jsonl(text), tasks, segments, a.set, excl, pages)
    with open(a.out, "w") as f:
        for r in out:
            f.write(json.dumps(r, sort_keys=True) + "\n")
    summ = {"set": a.set, "counts": c, "excluded_methods": sorted(excl),
            "pages_labelled": len({r["page_id"] for r in out})}
    if a.summary:
        json.dump(summ, open(a.summary, "w"), indent=1, sort_keys=True)
    print(json.dumps(summ, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
