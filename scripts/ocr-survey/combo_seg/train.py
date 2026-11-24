#!/usr/bin/env python3
"""Train the combined segmenter's classifier on labelled atoms (#2107). Training pages only.

A page-level dev split (sha256("2107:comboseg:dev:"+page id) order, --dev-frac of the pages) is held out from
fitting. On it the report gives the weighted AUC, label counts and permutation importances (sklearn, 5 repeats,
random_state 2107, scoring ROC AUC). With --refs, the keep threshold is chosen on the dev pages as the one with
the best pooled score.py primary F1 over a fixed grid; the final model is then refitted on all pages. Nothing
here reads a test page; the caller is responsible for giving training pages only.

    train.py --features a.jsonl [b.jsonl ...] --out-model model.pkl --report report.json
             [--dev-frac 0.2] [--refs refs.json [refs2.json ...]] [--source-weight negatives=0.5 ...]
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import feats  # noqa: E402
import gbdt  # noqa: E402
import score  # noqa: E402

GRID = [round(t, 2) for t in np.arange(0.1, 0.91, 0.05)]


def read_rows(paths):
    rows = []
    for p in paths:
        for ln in open(p):
            if ln.strip():
                rows.append(json.loads(ln))
    return rows


def dev_scores(rows, probs, refs):
    """Pooled score.py primary P, R, F1 on the dev pages at each grid threshold."""
    pids = sorted({r["page_id"] for r in rows} & set(refs))
    out = {}
    for t in GRID:
        g = gbdt.gutters(rows, probs, t)
        det = {p: {"gutters": g.get(p, [])} for p in pids}
        pr = score.page_rows(det, {p: refs[p] for p in pids})
        v = score._vec([pr[p] for p in pids], "primary")
        P, R = score.set_pr(v)
        out[t] = {"precision": P, "recall": R, "f1": 2 * P * R / (P + R) if P + R > 0 else 0.0}
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--features", nargs="+", required=True)
    ap.add_argument("--out-model", required=True)
    ap.add_argument("--report", required=True)
    ap.add_argument("--dev-frac", type=float, default=0.2)
    ap.add_argument("--refs", nargs="*", default=[])
    ap.add_argument("--source-weight", action="append", default=[], help="NAME=WEIGHT for a label source")
    ap.add_argument("--importance-repeats", type=int, default=5)
    a = ap.parse_args(argv)
    sw = {k: float(v) for k, v in (s.split("=", 1) for s in a.source_weight)}
    rows = read_rows(a.features)
    dev = gbdt.dev_split([r["page_id"] for r in rows], a.dev_frac)
    tr_rows = [r for r in rows if r["page_id"] not in dev]
    dv_rows = [r for r in rows if r["page_id"] in dev]

    def counts(rs):
        c = {}
        for r in rs:
            k = "%s:%s" % (r.get("label_source") or "unlabelled", r["label"])
            c[k] = c.get(k, 0) + 1
        return dict(sorted(c.items()))
    rep = {"features": list(feats.FEATURES), "params": gbdt.PARAMS, "source_weights": sw, "dev_frac": a.dev_frac,
           "pages": {"train": len({r["page_id"] for r in tr_rows}), "dev": len(dev)},
           "atoms": {"train": counts(tr_rows), "dev": counts(dv_rows)}}
    clf = gbdt.fit(tr_rows, sw)
    lab_dv = [r for r in dv_rows if r["label"] in (0, 1)]
    if lab_dv:
        from sklearn.inspection import permutation_importance
        X, y, w = feats.matrix(lab_dv)
        p = clf.predict_proba(X)[:, 1]
        rep["dev_auc_weighted"] = gbdt.auc(y, p, w)
        if len(set(y.tolist())) == 2:
            pi = permutation_importance(clf, X, y, sample_weight=w, scoring="roc_auc", n_repeats=a.importance_repeats,
                                        random_state=gbdt.SEED)
            imp = sorted(zip(feats.FEATURES, pi.importances_mean, pi.importances_std), key=lambda t: -t[1])
            rep["permutation_importance_dev_auc"] = [{"feature": f, "mean": float(m), "std": float(s)} for f, m, s in imp]
    refs = {}
    for path in a.refs:
        refs.update(json.load(open(path))["pages"])
    threshold = 0.5
    if refs and dv_rows:
        ds = dev_scores(dv_rows, gbdt.predict(clf, dv_rows), refs)
        rep["dev_threshold_grid"] = {str(k): v for k, v in ds.items()}
        threshold = max(GRID, key=lambda t: (round(ds[t]["f1"], 6), -abs(t - 0.5)))
    rep["threshold"] = threshold
    final = gbdt.fit(rows, sw)
    gbdt.save(final, a.out_model, {"threshold": threshold, "pages": len({r["page_id"] for r in rows}),
                                   "features_files": [os.path.basename(p) for p in a.features], "source_weights": sw})
    with open(a.report, "w") as f:
        json.dump(gbdt.to_json(rep), f, indent=1, sort_keys=True)
    print(json.dumps({k: rep[k] for k in ("pages", "atoms", "threshold") if k in rep}, sort_keys=True))
    print("dev AUC (weighted): %s" % rep.get("dev_auc_weighted"))
    for e in rep.get("permutation_importance_dev_auc", [])[:15]:
        print("  %-28s %.4f +- %.4f" % (e["feature"], e["mean"], e["std"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
