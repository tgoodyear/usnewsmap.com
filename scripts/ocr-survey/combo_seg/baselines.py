#!/usr/bin/env python3
"""Baselines of the combined segmenter's frozen plan (#2107, comment 7131927738) and their dev precision.

v2 was not adopted, so the baselines are built on banded v1 (337fc0aa):
  combo  La Liberte-type pages: the combination at 31b624560, banded v1 plus American Stories `article` gutters
         (conf 0.01) where banding found none (diag.add_where_none), as combo_eval.py and v2common.combo_gutters;
  v1     English pages (and German, descriptive): banded v1 alone.
Gutters are drawn from detect.py's outputs (v1.json, as-boxes.json), in columns.py's format.

--dev-precision-out: the pooled score.py primary precision of each page's baseline on the dev pages of the given
feature files (gbdt.dev_split over the same page ids train.py sees, so the same dev pages), over the pages that
have a reference. That value is train.py's --baseline-dev-precision.

    baselines.py --set NAME=TYPE:DETECTIONS_DIR [--set ...] --out-dir D
                 [--features f.jsonl ... --refs refs.json ... --dev-frac 0.2 --dev-precision-out F]
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
for d in (HERE, os.path.join(HERE, "..", "layout_columns"), os.path.join(HERE, "..", "layout_diag"),
          os.path.join(HERE, "..", "layout_train")):
    sys.path.insert(0, d)
import gbdt  # noqa: E402
import score  # noqa: E402

TYPES = ("combo", "v1")


def baseline_gutters(kind, v1_gutters, as_boxes, size):
    import diag
    if kind == "v1":
        return v1_gutters
    if kind == "combo":
        art = diag.gutters_generic(as_boxes, size, 0.01, diag.ARTICLE, diag.OBSTACLE)
        return v1_gutters + diag.add_where_none(v1_gutters, art, size)
    raise ValueError(kind)


def build(kind, det_dir):
    v1 = json.load(open(os.path.join(det_dir, "v1.json")))
    v1.pop("_meta", None)
    asb = json.load(open(os.path.join(det_dir, "as-boxes.json"))) if kind == "combo" else {}
    out = {}
    for pid, d in sorted(v1.items()):
        size = (d["width"], d["height"])
        out[pid] = {"gutters": baseline_gutters(kind, d["gutters"], asb.get(pid, []), size),
                    "height": d["height"], "width": d["width"]}
    return out


def dev_precision(features, refs, baseline, dev_frac):
    ids = set()
    for p in features:
        for ln in open(p):
            if ln.strip():
                ids.add(json.loads(ln)["page_id"])
    dev = gbdt.dev_split(ids, dev_frac)
    pids = sorted(p for p in dev if p in refs)
    missing = [p for p in pids if p not in baseline]
    if missing:
        raise SystemExit("no baseline for dev pages: %s" % missing[:5])
    pr = score.page_rows({p: {"gutters": baseline[p]["gutters"]} for p in pids}, {p: refs[p] for p in pids})
    P, R = score.set_pr(score._vec([pr[p] for p in pids], "primary"))
    return {"dev_pages_with_reference": len(pids), "precision": P, "recall": R,
            "f1": 2 * P * R / (P + R) if P + R > 0 else 0.0}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--set", action="append", required=True, help="NAME=TYPE:DETECTIONS_DIR, TYPE combo or v1")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--features", nargs="*", default=[])
    ap.add_argument("--refs", nargs="*", default=[])
    ap.add_argument("--dev-frac", type=float, default=0.2)
    ap.add_argument("--dev-precision-out")
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    allb = {}
    for spec in a.set:
        name, rest = spec.split("=", 1)
        kind, d = rest.split(":", 1)
        if kind not in TYPES:
            raise SystemExit("unknown baseline type " + kind)
        b = build(kind, d)
        b_out = dict(b, _meta={"baseline": kind, "set": name})
        json.dump(b_out, open(os.path.join(a.out_dir, "baseline-%s.json" % name), "w"), sort_keys=True)
        allb.update(b)
        print("baseline %s (%s): %d pages" % (name, kind, len(b)), flush=True)
    if a.dev_precision_out:
        refs = {}
        for p in a.refs:
            refs.update(json.load(open(p))["pages"])
        res = dev_precision(a.features, refs, allb, a.dev_frac)
        res["sets"] = a.set
        json.dump(res, open(a.dev_precision_out, "w"), indent=1, sort_keys=True)
        print(json.dumps(res, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
