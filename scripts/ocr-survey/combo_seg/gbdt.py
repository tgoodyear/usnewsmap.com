#!/usr/bin/env python3
"""The combined segmenter's classifier (#2107): a small gradient-boosted tree model over atom features.

scikit-learn's HistGradientBoostingClassifier, fixed settings, random_state 2107, no early stopping (so a run
is deterministic for given rows). Rows are weighted by atom height as a share of page height (the column
metric pools heights), times a per-source weight. Missing features are NaN, which the model routes natively.
"""
import hashlib
import json
import pickle

import numpy as np

import feats

SEED = 2107
PARAMS = {"learning_rate": 0.05, "max_iter": 300, "max_leaf_nodes": 15, "min_samples_leaf": 40,
          "l2_regularization": 1.0, "early_stopping": False, "random_state": SEED}


def page_key(prefix, pid):
    return hashlib.sha256((prefix + pid).encode()).hexdigest()


def dev_split(page_ids, frac, prefix="2107:comboseg:dev:"):
    """Deterministic page-level split: the first frac of pages in sha256(prefix + id) order are dev."""
    order = sorted(set(page_ids), key=lambda p: (page_key(prefix, p), p))
    n = int(round(frac * len(order)))
    return set(order[:n])


def fit(rows, source_weights=None, params=None):
    from sklearn.ensemble import HistGradientBoostingClassifier
    lab = [r for r in rows if r["label"] in (0, 1)]
    X, y, w = feats.matrix(lab)
    sw = source_weights or {}
    w = w * np.array([sw.get(r.get("label_source"), 1.0) for r in lab])
    clf = HistGradientBoostingClassifier(**dict(PARAMS, **(params or {})))
    clf.fit(X, y, sample_weight=w)
    return clf


def predict(clf, rows):
    if not rows:
        return np.zeros(0)
    X, _, _ = feats.matrix(rows)
    return clf.predict_proba(X)[:, 1]


def save(clf, path, info):
    import sklearn
    blob = {"model": clf, "features": list(feats.FEATURES), "sklearn": sklearn.__version__, "info": info}
    with open(path, "wb") as f:
        pickle.dump(blob, f)


def load(path):
    with open(path, "rb") as f:
        blob = pickle.load(f)
    if blob["features"] != list(feats.FEATURES):
        raise SystemExit("feature list of the saved model differs from feats.FEATURES")
    return blob


def gutters(rows, probs, threshold):
    """columns.py-format gutters per page from the kept atoms: one gutter per candidate, atoms sorted by y."""
    by = {}
    for r, p in zip(rows, probs):
        if p < threshold:
            continue
        by.setdefault(r["page_id"], {}).setdefault(r["cid"], []).append(r["piece"])
    out = {}
    for pid, cands in by.items():
        out[pid] = [sorted(v, key=lambda s: (s[2], s[0])) for _, v in sorted(cands.items())]
    return out


def auc(y, p, w=None):
    """Weighted ROC AUC (rank form), NaN when only one class is present."""
    y = np.asarray(y)
    p = np.asarray(p, dtype=float)
    w = np.ones(len(y)) if w is None else np.asarray(w, dtype=float)
    if len(set(y.tolist())) < 2:
        return float("nan")
    from sklearn.metrics import roc_auc_score
    return float(roc_auc_score(y, p, sample_weight=w))


def to_json(obj):
    return json.loads(json.dumps(obj, default=float))
