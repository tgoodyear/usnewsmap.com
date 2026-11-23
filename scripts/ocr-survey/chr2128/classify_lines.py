#!/usr/bin/env python3
"""Each S0 box called Cherokee or Latin with `chr_line_script.py`'s classifier as PR #1830 chose it: shape
features only (the scale-bound `h`, `w` and `contrast` left out), two stages (the second adds the first
stage's scores of the two lines above and below), trained on the LoC truth lines of the 160 pages outside
the #1622 sample (`feats-train-truth.jsonl`), threshold 0.5. `chr_line_script.py fit --predict-pages`
gives only per-page counts; this writes the per-line call with the same models.

    classify_lines.py --found s0.jsonl --scans DIR --layout ghn|loc --train feats-train-truth.jsonl
                      --out classified.jsonl [--jobs N]

Output: the found records with each line given "p_chr" (null when the crop has no measurable ink, which
the classifier skips) and "chr" (p_chr >= 0.5).
"""
import argparse
import concurrent.futures as cf
import json
import sys

import numpy as np

import common
import chr_line_script as ls  # noqa: E402

DROP = ("h", "w", "contrast")


def page_features(args):
    rec, scan = args
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    img = np.asarray(Image.open(scan).convert("L"))
    H, W = img.shape
    rows = []
    for i, l in enumerate(rec["lines"]):
        x0, y0, x1, y1 = l["box"]
        x0, y0, x1, y1 = max(x0, 0), max(y0, 0), min(x1, W), min(y1, H)
        if x1 - x0 < 8 or y1 - y0 < 8:
            continue
        try:
            f = ls.features(img[y0:y1, x0:x1])
        except ValueError:  # a crop of one grey level: otsu has no threshold; no ink, as features() returns None
            f = None
        if f is None:
            continue
        rows.append({"page": rec["page"], "i": i, "box": l["box"], "y": None, "f": [round(v, 4) for v in f]})
    return rows


def two_stage(tr, te):
    """predict_pages' models, returning the stage-two probability of each `te` row."""
    from sklearn.ensemble import HistGradientBoostingClassifier
    from sklearn.model_selection import GroupKFold
    Xtr, ytr = ls.mat(tr), np.array([r["y"] for r in tr])
    oof = np.zeros(len(tr))
    pages = np.array([r["page"] for r in tr])
    for f_tr, f_te in GroupKFold(ls.folds(pages)).split(Xtr, ytr, pages):
        m = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, random_state=0).fit(Xtr[f_tr], ytr[f_tr])
        oof[f_te] = m.predict_proba(Xtr[f_te])[:, 1]
    c1 = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, random_state=0).fit(Xtr, ytr)
    c2 = HistGradientBoostingClassifier(max_iter=200, learning_rate=0.08, random_state=0)
    c2.fit(np.hstack([Xtr, oof[:, None], ls.with_context(tr, oof)]), ytr)
    X = ls.mat(te)
    p1 = c1.predict_proba(X)[:, 1]
    return c2.predict_proba(np.hstack([X, p1[:, None], ls.with_context(te, p1)]))[:, 1]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--found", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--layout", choices=["ghn", "loc"], required=True)
    ap.add_argument("--train", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=1)
    a = ap.parse_args()
    ls.KEEP = [i for i, n in enumerate(ls.NAMES) if n not in DROP]
    found = common.read_jsonl(a.found)
    te = []
    with cf.ProcessPoolExecutor(a.jobs) as pool:
        for k, rows in enumerate(pool.map(page_features, [(r, common.scan_path(a.scans, r["page"], a.layout)) for r in found]), 1):
            te += rows
            if k % 25 == 0:
                print(f"features {k}/{len(found)} pages", file=sys.stderr, flush=True)
    tr = [r for r in ls.load([a.train]) if r["y"] is not None]
    print(f"train {len(tr)} lines, test {len(te)} lines", file=sys.stderr, flush=True)
    p = two_stage(tr, te)
    score = {(r["page"], r["i"]): float(v) for r, v in zip(te, p)}
    with open(a.out, "w") as out:
        for rec in found:
            for i, l in enumerate(rec["lines"]):
                v = score.get((rec["page"], i))
                l["p_chr"] = None if v is None else round(v, 5)
                l["chr"] = v is not None and v >= 0.5
            out.write(json.dumps(rec, sort_keys=True) + "\n")
    print(json.dumps({"pages": len(found), "lines": sum(len(r["lines"]) for r in found), "classified": len(te),
                      "chr": int(sum(v >= 0.5 for v in p))}), file=sys.stderr)


if __name__ == "__main__":
    main()
