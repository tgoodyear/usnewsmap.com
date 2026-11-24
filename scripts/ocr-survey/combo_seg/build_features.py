#!/usr/bin/env python3
"""Pool candidates, extract features and assign labels for the combined segmenter (#2107).

Reads detect.py's outputs (v1.json, as-boxes.json, optional tr-boxes.json, optional v2.json from the banded v2
finder), the page cache, optional word boxes (wordboxes.py) and one label source:
  --refs refs.json             reference pages (layout_columns/refs.py or cg_refs.py shape): positives and negatives
  --negatives negs.json        {page id: [[x0, y0, x1, y1, class], ...]} marked non-text boxes (Beyond Words):
                               negatives only
  --line-labels labels.jsonl   volunteer lines (export_engutter.py): {page_id, x_top, x_bottom, y0, y1, label}
With no label source the rows are unlabelled (inference). Writes one JSON line per atom (feats.page_rows).

    build_features.py --page-cache DIR --detections-dir DIR --out feats.jsonl [--pages-from FILE] [--refs FILE]
                      [--negatives FILE] [--line-labels FILE] [--words-dir DIR] [--set-name NAME] [--workers 2]
                      [--as-conf 0.01] [--tr-conf 0.25]
"""
import argparse
import json
import multiprocessing as mp
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
sys.path.insert(0, os.path.join(HERE, "..", "layout_diag"))
sys.path.insert(0, os.path.join(HERE, "..", "layout_train"))
import columns  # noqa: E402
import feats  # noqa: E402
import wordboxes  # noqa: E402

G = {}


def load_dets(d):
    def rd(name):
        p = os.path.join(d, name)
        if not os.path.exists(p):
            return None
        x = json.load(open(p))
        x.pop("_meta", None)
        return x
    return {"v1": rd("v1.json"), "v2": rd("v2.json"), "as": rd("as-boxes.json"), "tr": rd("tr-boxes.json")}


def page_inputs(pid, size):
    """Detector gutters and boxes of one page (gutters for as and tr are drawn from their boxes here)."""
    import diag
    d = G["dets"]
    det, boxes = {}, {}
    for k in ("v1", "v2"):
        if d[k] is not None:
            det[k] = d[k][pid]["gutters"]
    if d["as"] is not None:
        boxes["as"] = d["as"][pid]
        det["as"] = diag.gutters_generic(boxes["as"], size, G["as_conf"], diag.ARTICLE, diag.OBSTACLE)
    if d["tr"] is not None:
        boxes["tr"] = [b for b in d["tr"][pid] if b[4] >= G["tr_floor"]]
        det["tr"] = diag.gutters_generic(d["tr"][pid], size, G["tr_conf"], diag.ARTICLE, diag.OBSTACLE)
    return det, boxes


def _one(pid):
    gray, size = columns.load_cached(G["cache"], pid)
    det, boxes = page_inputs(pid, size)
    words = None
    if G["words_dir"]:
        p = os.path.join(G["words_dir"], wordboxes.out_name(pid))
        if os.path.exists(p):
            words = json.load(open(p))
    ref = G["refs"].get(pid) if G["refs"] is not None else None
    neg = G["negs"].get(pid, []) if G["negs"] is not None else None
    ll = G["lines"].get(pid, []) if G["lines"] is not None else None
    return feats.page_rows(pid, det, size, gray, boxes=boxes, words=words, ref=ref, neg_boxes=neg, line_labels=ll,
                           set_name=G["set_name"])


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--page-cache", required=True)
    ap.add_argument("--detections-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--pages-from", help="refs JSON or id list; default: the refs pages, else the v1 pages")
    ap.add_argument("--refs")
    ap.add_argument("--negatives")
    ap.add_argument("--line-labels")
    ap.add_argument("--words-dir")
    ap.add_argument("--set-name")
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--as-conf", type=float, default=0.01)
    ap.add_argument("--tr-conf", type=float, default=0.25)
    ap.add_argument("--tr-box-floor", type=float, default=0.01, help="trained-model boxes kept for the crossing features")
    a = ap.parse_args(argv)
    G.update(cache=a.page_cache, dets=load_dets(a.detections_dir), words_dir=a.words_dir, set_name=a.set_name,
             as_conf=a.as_conf, tr_conf=a.tr_conf, tr_floor=a.tr_box_floor, refs=None, negs=None, lines=None)
    if a.refs:
        G["refs"] = json.load(open(a.refs))["pages"]
    if a.negatives:
        G["negs"] = json.load(open(a.negatives))
    if a.line_labels:
        G["lines"] = {}
        for ln in open(a.line_labels):
            if ln.strip():
                r = json.loads(ln)
                G["lines"].setdefault(r["page_id"], []).append(r)
    if a.pages_from:
        ids = sorted(json.load(open(a.pages_from))["pages"]) if a.pages_from.endswith(".json") else \
            sorted(ln.strip() for ln in open(a.pages_from) if ln.strip())
    elif G["refs"] is not None:
        ids = sorted(G["refs"])
    else:
        ids = sorted(G["dets"]["v1"])
    t0 = time.time()
    n_rows = 0
    with open(a.out, "w") as f, mp.Pool(a.workers) as pool:
        for n, rows in enumerate(pool.imap(_one, ids), 1):
            for r in rows:
                f.write(json.dumps(r, sort_keys=True, allow_nan=True) + "\n")
            n_rows += len(rows)
            if n % 10 == 0 or n == len(ids):
                print("pages %d/%d atoms %d %.0fs" % (n, len(ids), n_rows, time.time() - t0), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
