"""Which script a text line is in, from its crop alone (#1622): syllabary or Latin.

For pages without ALTO, a finder gives boxes and the campaign needs each line sent to the right model
(`chr_phoenix` or `eng`). LoC's ALTO labels lines on the 190 pages we have, so a classifier is trained on
LoC's lines and scored on a finder's boxes.

  features  --truth truth.jsonl --scans DIR [--found found.jsonl] --out FILE.jsonl [--select keys.txt]
            One record per line: the page, the label, the shape features. Without --found, the lines are LoC's
            boxes. With it, the lines are the finder's boxes, labelled by the truth line each one matches
            (`chr_layout_eval.usable`); a box matching none is labelled null.
  fit       --train FILE.jsonl [--train FILE.jsonl ...] --test FILE.jsonl [--context | --predict-pages]
            A gradient-boosted classifier on the train records, scored on the test records. With --context a
            second stage adds the first stage's score for the two lines above and below.

The features are shape only, so they work on boxes of any tightness: after the crop is binarised, the ink's own
bounding box is used. Syllabary in this type is cast at capital height with few ascenders and descenders;
Latin lowercase has a body band with ascenders above and descenders below, and many small marks.
"""
import argparse
import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chr_layout_eval as ev  # noqa: E402

Image.MAX_IMAGE_PIXELS = None
NAMES = ["h", "w", "aspect", "n_cc_per_h", "cc_h_med", "cc_h_cv", "cc_small", "cc_tall", "cc_aspect",
         "density", "top", "mid", "bot", "body", "peak", "gaps_per_h", "gap_med", "cc_w_med", "contrast"]


def otsu(gray):
    hist = np.bincount(gray.ravel(), minlength=256).astype(float)
    total = hist.sum()
    sum_all = (np.arange(256) * hist).sum()
    w0 = np.cumsum(hist)
    s0 = np.cumsum(np.arange(256) * hist)
    w1 = total - w0
    with np.errstate(divide="ignore", invalid="ignore"):
        var = w0 * w1 * (s0 / w0 - (sum_all - s0) / w1) ** 2
    return int(np.nanargmax(var))


def features(gray):
    """Shape features of one line crop (uint8, dark ink on light paper); None when there is no ink."""
    if gray.shape[0] < 6 or gray.shape[1] < 12:
        return None
    thr = otsu(gray)
    ink = gray < thr
    contrast = float(gray[~ink].mean() - gray[ink].mean()) if ink.any() and (~ink).any() else 0.0
    lab, n = ndimage.label(ink, structure=np.ones((3, 3)))
    if n == 0:
        return None
    sizes = ndimage.sum(ink, lab, range(1, n + 1))
    keep = np.isin(lab, 1 + np.flatnonzero(sizes >= 5))
    if not keep.any():
        return None
    rows = keep.sum(1)
    cols = keep.sum(0)
    r = np.flatnonzero(rows >= 0.08 * rows.max())
    c = np.flatnonzero(cols > 0)
    top, bot = r[0], r[-1] + 1
    left, right = c[0], c[-1] + 1
    ink = keep[top:bot, left:right]
    H, W = ink.shape
    if H < 6 or W < 12:
        return None
    lab, n = ndimage.label(ink, structure=np.ones((3, 3)))
    objs = ndimage.find_objects(lab)
    cc_h = np.array([o[0].stop - o[0].start for o in objs], float)
    cc_w = np.array([o[1].stop - o[1].start for o in objs], float)
    prof = ink.sum(1).astype(float)
    third = prof.sum() or 1.0
    q = H // 4
    colp = ink.any(0)
    # runs of empty columns between ink: word gaps and letter gaps
    gaps, run = [], 0
    for v in colp:
        if v:
            if run:
                gaps.append(run)
            run = 0
        else:
            run += 1
    gaps = np.array(gaps, float) if gaps else np.zeros(1)
    return [
        float(H), float(W), W / H, n * H / W,
        float(np.median(cc_h)) / H, float(cc_h.std() / (cc_h.mean() or 1.0)),
        float((cc_h < 0.4 * H).mean()), float((cc_h > 0.85 * H).mean()), float(np.median(cc_w / cc_h)),
        float(ink.mean()),
        float(prof[:q].sum() / third), float(prof[q:H - q].sum() / third), float(prof[H - q:].sum() / third),
        float((prof >= 0.5 * prof.max()).mean()), float(prof.max() / (prof.mean() or 1.0)),
        float((gaps >= 0.35 * H).sum() * H / W), float(np.median(gaps[gaps > 0]) / H) if (gaps > 0).any() else 0.0,
        float(np.median(cc_w)) / H, contrast,
    ]


def label_of(truth_lines, box):
    hits = [l for l in truth_lines if ev.usable(l["box"], box)]
    if not hits:
        return None
    return int(any(l["chr"] for l in hits)) if len({l["chr"] for l in hits}) == 1 else None


def cmd_features(a):
    keys = {k.strip() for k in open(a.select) if k.strip()} if a.select else None
    found = {}
    if a.found:
        found = {r["page"]: r["lines"] for r in map(json.loads, open(a.found))}
    with open(a.out, "w") as out:
        for r in map(json.loads, open(a.truth)):
            page = r["page"]
            if (keys is not None and page not in keys) or (a.found and page not in found):
                continue
            img = np.asarray(Image.open(Path(a.scans) / f"{page.replace('/', '_')}.jpg").convert("L"))
            H, W = img.shape
            lines = found[page] if a.found else [{"box": l["box"]} for l in r["lines"]]
            for i, l in enumerate(lines):
                x0, y0, x1, y1 = l["box"]
                x0, y0, x1, y1 = max(x0, 0), max(y0, 0), min(x1, W), min(y1, H)
                if x1 - x0 < 8 or y1 - y0 < 8:
                    continue
                f = features(img[y0:y1, x0:x1])
                if f is None:
                    continue
                lab = int(r["lines"][i]["chr"]) if not a.found else label_of(r["lines"], l["box"])
                out.write(json.dumps({"page": page, "i": i, "box": l["box"], "y": lab,
                                      "f": [round(v, 4) for v in f]}) + "\n")


KEEP = list(range(len(NAMES)))


def mat(rows):
    return np.array([r["f"] for r in rows])[:, KEEP]


def load(paths):
    rows = []
    for p in paths:
        rows += [json.loads(l) for l in open(p)]
    return rows


def with_context(rows, score):
    """Adds the scores of the two lines above and the two below in the same column, by position."""
    out = np.zeros((len(rows), 4))
    by_page = {}
    for k, r in enumerate(rows):
        by_page.setdefault(r["page"], []).append(k)
    for ks in by_page.values():
        for k in ks:
            bx = rows[k]["box"]
            cx, cy = (bx[0] + bx[2]) / 2, (bx[1] + bx[3]) / 2
            near = []
            for j in ks:
                if j == k:
                    continue
                b = rows[j]["box"]
                if b[0] <= cx <= b[2] and abs((b[1] + b[3]) / 2 - cy) < 6 * (bx[3] - bx[1]):
                    near.append(((b[1] + b[3]) / 2 - cy, score[j]))
            above = sorted([n for n in near if n[0] < 0], key=lambda n: -n[0])[:2]
            below = sorted([n for n in near if n[0] > 0], key=lambda n: n[0])[:2]
            for slot, lst in ((0, above), (2, below)):
                for t in range(2):
                    out[k, slot + t] = lst[t][1] if t < len(lst) else -1.0
    return out


def predict_pages(tr, te):
    """Per page of `te` (any lines, labelled or not): how many lines the two-stage classifier calls syllabary."""
    from sklearn.ensemble import HistGradientBoostingClassifier
    from sklearn.model_selection import GroupKFold
    Xtr, ytr = mat(tr), np.array([r["y"] for r in tr])
    oof = np.zeros(len(tr))
    pages = np.array([r["page"] for r in tr])
    for f_tr, f_te in GroupKFold(5).split(Xtr, ytr, pages):
        m = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, random_state=0).fit(Xtr[f_tr], ytr[f_tr])
        oof[f_te] = m.predict_proba(Xtr[f_te])[:, 1]
    c1 = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, random_state=0).fit(Xtr, ytr)
    c2 = HistGradientBoostingClassifier(max_iter=200, learning_rate=0.08, random_state=0)
    c2.fit(np.hstack([Xtr, oof[:, None], with_context(tr, oof)]), ytr)
    X = mat(te)
    p1 = c1.predict_proba(X)[:, 1]
    p2 = c2.predict_proba(np.hstack([X, p1[:, None], with_context(te, p1)]))[:, 1]
    per = {}
    for r, p in zip(te, p2):
        d = per.setdefault(r["page"], {"lines": 0, "syllabary": 0})
        d["lines"] += 1
        d["syllabary"] += int(p >= 0.5)
    print(json.dumps({"pages": per, "lines": sum(d["lines"] for d in per.values()),
                      "syllabary": int(sum(d["syllabary"] for d in per.values()))}, indent=1))


def cmd_fit(a):
    global KEEP
    KEEP = [i for i, n in enumerate(NAMES) if n not in set(a.drop)]
    from sklearn.ensemble import HistGradientBoostingClassifier
    from sklearn.metrics import roc_auc_score
    tr, te = load(a.train), load([a.test])
    tr = [r for r in tr if r["y"] is not None]
    if a.predict_pages:
        return predict_pages(tr, te)
    Xtr, ytr = mat(tr), np.array([r["y"] for r in tr])
    te_l = [r for r in te if r["y"] is not None]
    Xte, yte = mat(te_l), np.array([r["y"] for r in te_l])
    clf = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, random_state=0).fit(Xtr, ytr)
    p = clf.predict_proba(Xte)[:, 1]
    res = {"train_lines": len(tr), "train_chr": int(ytr.sum()), "test_lines": len(te_l), "test_chr": int(yte.sum())}

    def report(p, name):
        pred = p >= 0.5
        tp = int((pred & (yte == 1)).sum())
        fp = int((pred & (yte == 0)).sum())
        fn = int((~pred & (yte == 1)).sum())
        res[name] = {"auc": round(float(roc_auc_score(yte, p)), 5),
                     "chr_recall": round(tp / max(tp + fn, 1), 4), "chr_precision": round(tp / max(tp + fp, 1), 4),
                     "errors": fp + fn, "chr_as_eng": fn, "eng_as_chr": fp,
                     "accuracy": round(1 - (fp + fn) / len(yte), 4)}
    report(p, "shape")
    if a.context:
        # stage two: out-of-fold stage-one scores on the train set, so it does not learn from its own fit
        from sklearn.model_selection import GroupKFold
        pages = np.array([r["page"] for r in tr])
        oof = np.zeros(len(tr))
        for f_tr, f_te in GroupKFold(5).split(Xtr, ytr, pages):
            m = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, random_state=0).fit(Xtr[f_tr], ytr[f_tr])
            oof[f_te] = m.predict_proba(Xtr[f_te])[:, 1]
        c2 = HistGradientBoostingClassifier(max_iter=200, learning_rate=0.08, random_state=0)
        c2.fit(np.hstack([Xtr, oof[:, None], with_context(tr, oof)]), ytr)
        p_all = clf.predict_proba(mat(te))[:, 1]
        ctx = with_context(te, p_all)
        keep = [k for k, r in enumerate(te) if r["y"] is not None]
        p2 = c2.predict_proba(np.hstack([Xte, p_all[keep][:, None], ctx[keep]]))[:, 1]
        report(p2, "shape_plus_neighbours")
    print(json.dumps(res, indent=1))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("features")
    f.add_argument("--truth", required=True)
    f.add_argument("--scans", required=True)
    f.add_argument("--found")
    f.add_argument("--select")
    f.add_argument("--out", required=True)
    f.set_defaults(fn=cmd_features)
    t = sub.add_parser("fit")
    t.add_argument("--train", action="append", required=True)
    t.add_argument("--test", required=True)
    t.add_argument("--context", action="store_true")
    t.add_argument("--drop", action="append", default=[], choices=NAMES,
                   help="leave a feature out (the scale-bound ones: h, w, contrast, when scans differ in size)")
    t.add_argument("--predict-pages", action="store_true", help="count the lines called syllabary on each page of --test")
    t.set_defaults(fn=cmd_fit)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
