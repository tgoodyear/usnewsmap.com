#!/usr/bin/env python3
"""Non-text mask test for #2107 (pre-registered): can the American Stories layout model supply a mask?

Model: American Stories layout detector (Dell et al., NeurIPS 2023 Datasets and Benchmarks,
arXiv:2308.12477; code https://github.com/dell-research-harvard/AmericanStories), ONNX mirror
NealCaren/american-stories-onnx at 2558a023, file layout_model_new.onnx. Research use only; the weights
are never copied into the repository. Test set: the 712-page validation split of Beyond Words, Lee et
al., CIKM 2020, arXiv:2005.01583 (LoC, biglam/loc_beyond_words at 6c7f5fb3, CC0). Train is not used.

Inference is run_shard.py (it reuses layout_columns/asmodel.py unchanged: upstream yolov8 settings, conf
0.01, IoU 0.1, agnostic NMS, max_det 2000). This file scores the boxes. Nothing is tuned.

Class mapping (fixed in the pre-registration):
  photograph                 <- Photograph
  cartoon_or_advertisement   <- Illustration, Map, Comics/Cartoon, Editorial Cartoon, Advertisement
  headline                   <- Headline
Non-text = photograph + cartoon_or_advertisement pooled. Headlines are neither reference nor prediction
in the primary measures. masthead, newspaper_header, table and the text classes are reported separately.

Measures (predictions with confidence >= 0.25):
  - mask recall: share of reference non-text area covered by predicted non-text boxes;
  - mask precision: share of predicted non-text area inside reference non-text boxes.
  Boxes are merged as a union per page before area is counted; areas are summed over pages and the
  ratio is taken once. Intervals: page bootstrap, 1,000 draws, seed 2107 (numpy default_rng).
  - box AP at IoU >= 0.5 with COCO-style 101-point interpolation (written here, no library; matching is
    greedy by confidence to the best unmatched reference box, and there is no cap on detections per page),
    and box recall at the 0.25 cut. Reported only.
Decision rule: use the model's non-text boxes as the corpus mask only if recall >= 0.80 and precision >= 0.70.

    nontext.py --rows VAL.jsonl --boxes shard0.json --boxes shard1.json ... --out result.json
"""
import argparse
import json
import sys

import numpy as np

CUT = 0.25
SEED = 2107
DRAWS = 1000
IOU = 0.5
PHOTO, CARTOON, HEADLINE = "photograph", "cartoon_or_advertisement", "headline"
BW_TO_AS = {"Photograph": PHOTO, "Illustration": CARTOON, "Map": CARTOON, "Comics/Cartoon": CARTOON,
            "Editorial Cartoon": CARTOON, "Advertisement": CARTOON, "Headline": HEADLINE}
NONTEXT = (PHOTO, CARTOON)
OTHER_CLASSES = ("masthead", "newspaper_header", "table", "article", "author", "image_caption", "page_number")
# score sets: name -> the American Stories classes taken as reference and as prediction
SETS = {"nontext": NONTEXT, "photograph": (PHOTO,), "cartoon_or_advertisement": (CARTOON,),
        "headline": (HEADLINE,)}


def union_area(boxes):
    """Area of the union of [x0, y0, x1, y1] boxes, by sweeping over compressed x coordinates."""
    b = [bx for bx in boxes if bx[2] > bx[0] and bx[3] > bx[1]]
    if not b:
        return 0.0
    xs = sorted({v for bx in b for v in (bx[0], bx[2])})
    total = 0.0
    for x0, x1 in zip(xs, xs[1:]):
        spans = sorted((bx[1], bx[3]) for bx in b if bx[0] <= x0 and bx[2] >= x1)
        cov, start, end = 0.0, None, None
        for y0, y1 in spans:
            if end is None or y0 > end:
                if end is not None:
                    cov += end - start
                start, end = y0, y1
            elif y1 > end:
                end = y1
        if end is not None:
            cov += end - start
        total += cov * (x1 - x0)
    return total


def inter_union_area(a, b):
    """Area of (union of a) intersected with (union of b)."""
    return union_area(a) + union_area(b) - union_area(list(a) + list(b))


def iou(p, q):
    iw = min(p[2], q[2]) - max(p[0], q[0])
    ih = min(p[3], q[3]) - max(p[1], q[1])
    if iw <= 0 or ih <= 0:
        return 0.0
    i = iw * ih
    return i / ((p[2] - p[0]) * (p[3] - p[1]) + (q[2] - q[0]) * (q[3] - q[1]) - i)


def ref_boxes(row):
    """Reference boxes per American Stories class from one converted row (conv_locbw.py output)."""
    out = {PHOTO: [], CARTOON: [], HEADLINE: []}
    for r in list(row.get("nontext_regions", [])) + list(row.get("text_regions", [])):
        c = BW_TO_AS.get(r["class"])
        if c is None:
            raise ValueError("unmapped Beyond Words class %r on page %s" % (r["class"], row["page_id"]))
        out[c].append([float(v) for v in r["box"]])
    return out


def pred_boxes(dets, cut=CUT):
    """Predicted boxes (confidence >= cut) per class from [x0, y0, x1, y1, conf, name] rows."""
    out = {}
    for d in dets:
        if d[4] >= cut:
            out.setdefault(d[5], []).append([float(v) for v in d[:4]])
    return out


def match_page(gt, dets):
    """COCO greedy matching at IoU >= IOU. dets: (conf, box). Returns (confs, tp flags) in confidence order."""
    dets = sorted(dets, key=lambda d: -d[0])
    used = [False] * len(gt)
    confs, tp = [], []
    for conf, box in dets:
        best, bj = IOU, -1
        for j, g in enumerate(gt):
            if used[j]:
                continue
            v = iou(box, g)
            if v >= best:
                best, bj = v, j
        if bj >= 0:
            used[bj] = True
        confs.append(conf)
        tp.append(bj >= 0)
    return np.array(confs, dtype=float), np.array(tp, dtype=bool)


def average_precision(confs, tp, n_gt):
    """COCO 101-point interpolated AP from pooled detections."""
    if n_gt == 0:
        return float("nan")
    if len(confs) == 0:
        return 0.0
    order = np.argsort(-confs, kind="mergesort")
    t = tp[order].astype(float)
    ctp, cfp = np.cumsum(t), np.cumsum(1 - t)
    rec = ctp / n_gt
    prec = ctp / (ctp + cfp)
    for i in range(len(prec) - 1, 0, -1):
        prec[i - 1] = max(prec[i - 1], prec[i])
    qs = np.zeros(101)
    idx = np.searchsorted(rec, np.linspace(0, 1, 101), side="left")
    for k, i in enumerate(idx):
        if i < len(prec):
            qs[k] = prec[i]
    return float(qs.mean())


def page_stats(refs, dets):
    """Per-page numbers for every score set; refs from ref_boxes, dets raw detections of the page."""
    pred_cut = pred_boxes(dets, CUT)
    s = {}
    for name, classes in SETS.items():
        rb = [b for c in classes for b in refs[c]]
        pb = [b for c in classes for b in pred_cut.get(c, [])]
        ra, pa = union_area(rb), union_area(pb)
        ia = ra + pa - union_area(rb + pb)
        alld = [(d[4], d[:4]) for d in dets if d[5] in classes]
        confs, tp = match_page(rb, alld)
        s[name] = {"ref_area": ra, "pred_area": pa, "inter_area": ia, "n_gt": len(rb), "confs": confs, "tp": tp}
    non_ref = [b for c in NONTEXT for b in refs[c]]
    other = {}
    for c in OTHER_CLASSES:
        pb = pred_cut.get(c, [])
        other[c] = {"n": len(pb), "pred_area": union_area(pb),
                    "inside_ref_nontext": inter_union_area(pb, non_ref) if pb else 0.0}
    s["_other"] = other
    return s


def totals(stats, idx):
    """Pooled figures for one score set over the pages in idx (indices into the stats list)."""
    ra = sum(stats[i]["ref_area"] for i in idx)
    pa = sum(stats[i]["pred_area"] for i in idx)
    ia = sum(stats[i]["inter_area"] for i in idx)
    n_gt = sum(stats[i]["n_gt"] for i in idx)
    confs = np.concatenate([stats[i]["confs"] for i in idx]) if len(idx) else np.zeros(0)
    tp = np.concatenate([stats[i]["tp"] for i in idx]) if len(idx) else np.zeros(0, dtype=bool)
    sel = confs >= CUT
    return {"mask_recall": ia / ra if ra else float("nan"), "mask_precision": ia / pa if pa else float("nan"),
            "box_ap": average_precision(confs, tp, n_gt),
            "box_recall": float(tp[sel].sum()) / n_gt if n_gt else float("nan"),
            "box_precision": float(tp[sel].sum()) / int(sel.sum()) if sel.any() else float("nan"),
            "n_ref_boxes": n_gt, "n_pred_boxes": int(sel.sum()),
            "ref_area": ra, "pred_area": pa, "inter_area": ia}


def bootstrap(per_set, n_pages, draws=DRAWS, seed=SEED):
    """Percentile 95% intervals of every pooled figure of every set, page bootstrap with a shared draw."""
    rng = np.random.default_rng(seed)
    keys = ("mask_recall", "mask_precision", "box_ap", "box_recall", "box_precision")
    acc = {n: {k: [] for k in keys} for n in per_set}
    for _ in range(draws):
        idx = rng.integers(0, n_pages, n_pages)
        for n, st in per_set.items():
            t = totals(st, idx)
            for k in keys:
                acc[n][k].append(t[k])
    out = {}
    for n in per_set:
        out[n] = {}
        for k in keys:
            v = np.array(acc[n][k], dtype=float)
            v = v[~np.isnan(v)]
            out[n][k] = [float(np.percentile(v, 2.5)), float(np.percentile(v, 97.5))] if len(v) else [None, None]
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--rows", required=True, help="Beyond Words validation rows (conv_locbw.py JSONL)")
    ap.add_argument("--boxes", action="append", required=True, help="shard output of run_shard.py (repeatable)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--draws", type=int, default=DRAWS)
    a = ap.parse_args(argv)
    rows = {}
    for ln in open(a.rows):
        r = json.loads(ln)
        rows[r["page_id"]] = r
    dets, skipped, meta = {}, {}, []
    for p in a.boxes:
        d = json.load(open(p))
        meta.append(d["_meta"])
        dets.update(d["pages"])
        skipped.update(d["skipped"])
    missing = sorted(set(rows) - set(dets) - set(skipped))
    if missing:
        raise SystemExit("pages with neither detections nor a skip reason: %s" % missing[:10])
    ids = sorted(dets)
    per_page = [page_stats(ref_boxes(rows[i]), dets[i]) for i in ids]
    per_set = {n: [p[n] for p in per_page] for n in SETS}
    allidx = list(range(len(ids)))
    res = {"decision_rule": {"mask_precision_min": 0.70, "mask_recall_min": 0.80},
           "pages_scored": len(ids), "pages_in_split": len(rows), "skipped": skipped,
           "cut": CUT, "iou": IOU, "seed": SEED, "draws": a.draws, "shards": meta}
    ci = bootstrap(per_set, len(ids), a.draws)
    res["sets"] = {}
    for n in SETS:
        t = totals(per_set[n], allidx)
        t["ci95"] = ci[n]
        res["sets"][n] = t
    res["other_classes_at_cut"] = {
        c: {"n_boxes": sum(p["_other"][c]["n"] for p in per_page),
            "pred_area": sum(p["_other"][c]["pred_area"] for p in per_page),
            "inside_ref_nontext": sum(p["_other"][c]["inside_ref_nontext"] for p in per_page)}
        for c in OTHER_CLASSES}
    nt = res["sets"]["nontext"]
    res["decision"] = {"passes": bool(nt["mask_recall"] >= 0.80 and nt["mask_precision"] >= 0.70),
                       "precision_ok": bool(nt["mask_precision"] >= 0.70), "recall_ok": bool(nt["mask_recall"] >= 0.80)}
    with open(a.out, "w") as f:
        json.dump(res, f, sort_keys=True, indent=1)
    print(json.dumps({k: nt[k] for k in ("mask_recall", "mask_precision", "box_ap", "box_recall", "ci95")}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
