#!/usr/bin/env python3
"""Out-of-fold p_chr for the clf2 fitting set (#2128 Amendment 3).

The current classifier (`classify_lines.py`) was trained on the LoC truth lines of 160 pages that include
all 154 layout-test pages, so its p_chr on those pages is in-sample. This gives each fitting page the p_chr
of the same two-stage classifier trained without that page's fold: the 154 pages are split into --folds
groups by sha256("2128:clf2-oof:"+page), and each group is scored by a classifier trained on the training
lines of every other page. Same features, models and output format as classify_lines.py.

    oof_pchr.py --found s1.jsonl --scans DIR --train feats-train-truth.jsonl --pages layout-test-pages.txt
                --out s1-classified-oof.jsonl [--folds 5] [--jobs N]
"""
import argparse
import concurrent.futures as cf
import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import classify_lines as cl  # noqa: E402
import common  # noqa: E402

ls = cl.ls


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for k in ("--found", "--scans", "--train", "--pages", "--out"):
        ap.add_argument(k, required=True)
    ap.add_argument("--folds", type=int, default=5)
    ap.add_argument("--jobs", type=int, default=1)
    a = ap.parse_args()
    ls.KEEP = [i for i, n in enumerate(ls.NAMES) if n not in cl.DROP]
    pages = common.read_pages(a.pages)
    fold = {p: int(hashlib.sha256(("2128:clf2-oof:" + p).encode()).hexdigest(), 16) % a.folds for p in pages}
    found = [r for r in common.read_jsonl(a.found) if r["page"] in fold]
    te = []
    with cf.ProcessPoolExecutor(a.jobs) as pool:
        for rows in pool.map(cl.page_features, [(r, common.scan_path(a.scans, r["page"], "loc")) for r in found]):
            te += rows
    tr_all = [r for r in ls.load([a.train]) if r["y"] is not None]
    score = {}
    for k in range(a.folds):
        tr = [r for r in tr_all if fold.get(r["page"]) != k]
        tk = [r for r in te if fold[r["page"]] == k]
        for r, v in zip(tk, cl.two_stage(tr, tk)):
            score[(r["page"], r["i"])] = float(v)
        print(f"fold {k}: train {len(tr)} lines, scored {len(tk)}", file=sys.stderr, flush=True)
    with open(a.out, "w") as out:
        for rec in found:
            for i, l in enumerate(rec["lines"]):
                v = score.get((rec["page"], i))
                l["p_chr"] = None if v is None else round(v, 5)
                l["chr"] = v is not None and v >= 0.5
            out.write(json.dumps(rec, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
